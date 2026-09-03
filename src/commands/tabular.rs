use serde::Serialize;

use fractal::sap::table::TableColumn;

const MAX_READABLE_CELL_WIDTH: usize = 40;

#[derive(Debug, Serialize)]
pub(super) struct ColumnOutput {
    pub(super) name: String,
    pub(super) sap_type: Option<String>,
    pub(super) col_type: Option<String>,
    pub(super) length: Option<u32>,
    pub(super) description: Option<String>,
}

pub(super) fn map_columns(columns: Vec<TableColumn>) -> Vec<ColumnOutput> {
    columns
        .into_iter()
        .map(|column| ColumnOutput {
            name: column.name,
            sap_type: column.sap_type,
            col_type: column.col_type,
            length: column.length,
            description: column.description,
        })
        .collect()
}

/// A grid column that is only a heading: the metadata fields describe SAP
/// table columns and mean nothing for a rendered-only grid.
pub(super) fn plain_column(name: &str) -> ColumnOutput {
    ColumnOutput {
        name: name.to_owned(),
        sap_type: None,
        col_type: None,
        length: None,
        description: None,
    }
}

pub(super) fn render_grid(columns: &[ColumnOutput], rows: &[Vec<String>]) -> String {
    if columns.is_empty() {
        return "(no columns)\n".to_owned();
    }

    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|cell| normalize_cell(cell)).collect())
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|cell| cell.chars().count())
                .chain(std::iter::once(column.name.chars().count()))
                .max()
                .unwrap_or(1)
                .clamp(1, MAX_READABLE_CELL_WIDTH)
        })
        .collect();
    let headers: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
    let mut output = String::new();
    write_row(&mut output, &headers, &widths);
    let separators: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    write_row(&mut output, &separators, &widths);
    for row in &rows {
        write_row(&mut output, row, &widths);
    }
    output
}

fn normalize_cell(cell: &str) -> String {
    cell.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn write_row(output: &mut String, cells: &[String], widths: &[usize]) {
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        let value = cells.get(index).map_or("", String::as_str);
        let value = truncate_cell(value, width);
        output.push_str(&value);
        output.push_str(&" ".repeat(width.saturating_sub(value.chars().count())));
    }
    output.push('\n');
}

fn truncate_cell(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }

    value.chars().take(width - 1).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rows_and_normalizes_multiline_cells() {
        let columns = vec![ColumnOutput {
            name: "NOTE".to_owned(),
            sap_type: None,
            col_type: None,
            length: None,
            description: None,
        }];
        let rendered = render_grid(&columns, &[vec!["first\nsecond".to_owned()]]);

        assert!(rendered.contains("NOTE"));
        assert!(rendered.contains("first second"));
    }
}
