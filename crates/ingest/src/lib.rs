//! Host-side tabular I/O: CSV/XLSX <-> row-oriented records.
//!
//! Records are `IndexMap<String, serde_json::Value>` (insertion-ordered, so
//! column order round-trips). This crate is pure Rust and has no knowledge
//! of Python; it only produces/consumes plain records.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use calamine::{open_workbook_auto, Data, Reader};
use indexmap::IndexMap;
use serde_json::Value;

pub type Record = IndexMap<String, Value>;

/// A file's first row (usable either as column names or, for a headerless
/// read, as an ordinary data row) plus the rows after it.
pub struct Table {
    /// First row's cells, as strings -- column names when the file has a
    /// header.
    pub header_names: Vec<String>,
    /// First row's cells, scalar-inferred like any other cell -- used as an
    /// ordinary data row for a headerless read.
    pub header_values: Vec<Value>,
    /// Every row after the first, scalar-inferred, aligned by column index.
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    /// Named records, using the first row as column names (the default).
    pub fn to_records(&self) -> Vec<Record> {
        self.rows
            .iter()
            .map(|row| {
                self.header_names
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.clone(), row.get(i).cloned().unwrap_or(Value::Null)))
                    .collect()
            })
            .collect()
    }

    /// Every row including the first, as plain value lists -- for a
    /// headerless read, where pandas assigns integer column names 0..n.
    pub fn to_headerless_rows(&self) -> Vec<Vec<Value>> {
        let mut all = Vec::with_capacity(self.rows.len() + 1);
        all.push(self.header_values.clone());
        all.extend(self.rows.iter().cloned());
        all
    }
}

/// Read a CSV or XLSX file into row-oriented records, dispatching on extension.
pub fn read_records(path: &Path) -> Result<Vec<Record>> {
    Ok(read_table(path)?.to_records())
}

/// Read a CSV or XLSX file into a `Table`, dispatching on extension.
pub fn read_table(path: &Path) -> Result<Table> {
    match extension_lower(path).as_deref() {
        Some("csv") => read_csv_table(path),
        Some("xlsx") | Some("xlsm") | Some("xls") => read_xlsx_table(path, None),
        other => bail!(
            "unsupported input extension {:?} for {}; expected csv/xlsx",
            other,
            path.display()
        ),
    }
}

pub fn read_csv(path: &Path) -> Result<Vec<Record>> {
    Ok(read_csv_table(path)?.to_records())
}

fn read_csv_table(path: &Path) -> Result<Table> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(BufReader::new(file));

    let header_names: Vec<String> = rdr
        .headers()
        .with_context(|| format!("reading header row of {}", path.display()))?
        .iter()
        .map(|s| s.to_string())
        .collect();
    let header_values: Vec<Value> = header_names.iter().map(|s| infer_scalar(s)).collect();

    let mut rows = Vec::new();
    for result in rdr.records() {
        let row = result.with_context(|| format!("reading row of {}", path.display()))?;
        rows.push((0..header_names.len()).map(|i| infer_scalar(row.get(i).unwrap_or(""))).collect());
    }
    Ok(Table {
        header_names,
        header_values,
        rows,
    })
}

/// Read the first sheet (or a named sheet) of an XLSX/XLS/XLSM workbook.
pub fn read_xlsx(path: &Path, sheet_name: Option<&str>) -> Result<Vec<Record>> {
    Ok(read_xlsx_table(path, sheet_name)?.to_records())
}

fn read_xlsx_table(path: &Path, sheet_name: Option<&str>) -> Result<Table> {
    let mut workbook =
        open_workbook_auto(path).with_context(|| format!("opening {}", path.display()))?;

    let sheet_name = match sheet_name {
        Some(name) => name.to_string(),
        None => workbook
            .sheet_names()
            .first()
            .cloned()
            .with_context(|| format!("{} has no sheets", path.display()))?,
    };

    let range = workbook
        .worksheet_range(&sheet_name)
        .with_context(|| format!("reading sheet {:?} of {}", sheet_name, path.display()))?;

    let mut rows_iter = range.rows();
    let header_row = match rows_iter.next() {
        Some(row) => row,
        None => {
            return Ok(Table {
                header_names: Vec::new(),
                header_values: Vec::new(),
                rows: Vec::new(),
            })
        }
    };
    let header_names: Vec<String> = header_row.iter().map(cell_to_string).collect();
    let header_values: Vec<Value> = header_row.iter().map(cell_to_value).collect();

    let mut rows = Vec::new();
    for row in rows_iter {
        rows.push(
            (0..header_names.len())
                .map(|i| row.get(i).map(cell_to_value).unwrap_or(Value::Null))
                .collect(),
        );
    }
    Ok(Table {
        header_names,
        header_values,
        rows,
    })
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn cell_to_value(cell: &Data) -> Value {
    match cell {
        Data::Empty => Value::Null,
        Data::String(s) => Value::String(s.clone()),
        Data::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Data::Int(i) => Value::Number((*i).into()),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(dt) => Value::Number(
            serde_json::Number::from_f64(dt.as_f64()).unwrap_or_else(|| 0.into()),
        ),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Value::String(s.clone()),
        Data::Error(e) => Value::String(format!("#ERROR: {e:?}")),
    }
}

/// Best-effort scalar type inference for a raw CSV cell, mirroring the kind
/// of light inference a dataframe library does on load (int/float/bool/null,
/// else string). Kept intentionally simple.
fn infer_scalar(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = raw.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    match raw {
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    Value::String(raw.to_string())
}

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
}

/// Output format for writing records back out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Csv,
    Xlsx,
    Ndjson,
    Table,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_lowercase().as_str() {
            "csv" => OutputFormat::Csv,
            "xlsx" => OutputFormat::Xlsx,
            "ndjson" | "jsonl" => OutputFormat::Ndjson,
            "table" => OutputFormat::Table,
            other => bail!("unknown output format {other:?}; expected csv/xlsx/ndjson/table"),
        })
    }

    /// Infer a format from an output path's extension, defaulting to csv.
    pub fn infer_from_path(path: &Path) -> Self {
        match extension_lower(path).as_deref() {
            Some("xlsx") => OutputFormat::Xlsx,
            Some("ndjson") | Some("jsonl") => OutputFormat::Ndjson,
            _ => OutputFormat::Csv,
        }
    }
}

/// Collect the union of keys across records, preserving first-seen order.
fn column_order(records: &[Record]) -> Vec<String> {
    let mut order = Vec::new();
    for record in records {
        for key in record.keys() {
            if !order.contains(key) {
                order.push(key.clone());
            }
        }
    }
    order
}

fn value_to_cell_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

pub fn write_records(records: &[Record], format: OutputFormat, mut out: impl Write) -> Result<()> {
    match format {
        OutputFormat::Csv => write_csv(records, out),
        OutputFormat::Ndjson => write_ndjson(records, out),
        OutputFormat::Table => write_table(records, &mut out),
        OutputFormat::Xlsx => bail!("xlsx output must be written to a file path, not a stream"),
    }
}

fn write_csv(records: &[Record], out: impl Write) -> Result<()> {
    let columns = column_order(records);
    let mut wtr = csv::Writer::from_writer(out);
    wtr.write_record(&columns)?;
    for record in records {
        let row: Vec<String> = columns
            .iter()
            .map(|c| record.get(c).map(value_to_cell_string).unwrap_or_default())
            .collect();
        wtr.write_record(&row)?;
    }
    wtr.flush()?;
    Ok(())
}

fn write_ndjson(records: &[Record], mut out: impl Write) -> Result<()> {
    for record in records {
        serde_json::to_writer(&mut out, record)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

fn write_table(records: &[Record], out: &mut impl Write) -> Result<()> {
    let columns = column_order(records);
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    let rendered: Vec<Vec<String>> = records
        .iter()
        .map(|record| {
            columns
                .iter()
                .map(|c| record.get(c).map(value_to_cell_string).unwrap_or_default())
                .collect()
        })
        .collect();
    for row in &rendered {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let write_row = |out: &mut dyn Write, cells: &[String]| -> io::Result<()> {
        let line = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{:width$}", c, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ");
        writeln!(out, "{}", line.trim_end())
    };
    write_row(out, &columns)?;
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    write_row(out, &sep)?;
    for row in &rendered {
        write_row(out, row)?;
    }
    Ok(())
}

pub fn write_xlsx(records: &[Record], path: &Path) -> Result<()> {
    use rust_xlsxwriter::Workbook;

    let columns = column_order(records);
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();

    for (col, name) in columns.iter().enumerate() {
        sheet.write_string(0, col as u16, name)?;
    }
    for (row_idx, record) in records.iter().enumerate() {
        for (col, name) in columns.iter().enumerate() {
            let row = (row_idx + 1) as u32;
            match record.get(name) {
                Some(Value::Number(n)) => {
                    if let Some(f) = n.as_f64() {
                        sheet.write_number(row, col as u16, f)?;
                    }
                }
                Some(Value::Bool(b)) => {
                    sheet.write_boolean(row, col as u16, *b)?;
                }
                Some(Value::Null) | None => {}
                Some(other) => {
                    sheet.write_string(row, col as u16, &value_to_cell_string(other))?;
                }
            }
        }
    }
    workbook.save(path)?;
    Ok(())
}
