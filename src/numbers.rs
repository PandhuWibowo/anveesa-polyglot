use anyhow::{bail, Context, Result};
use std::process::Command;

pub struct SheetInfo {
    pub doc_name: String,
    pub sheet_name: String,
    pub rows: usize,
    pub cols: usize,
}

/// One text-bearing, non-formula cell found on the active sheet's first table.
pub struct SheetCell {
    pub cell_ref: String,
    pub text: String,
}

fn run_applescript(script: &str) -> Result<String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("running osascript (is Numbers installed?)")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        if err.contains("(-1728)") || err.contains("Can't get") {
            bail!("no active sheet/table found — is a spreadsheet open in Numbers?");
        }
        if err.contains("(-600)") || err.to_lowercase().contains("not running") {
            bail!("Numbers isn't running — open your spreadsheet in Numbers first");
        }
        bail!("Numbers automation failed: {}", err.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 0-indexed column number → spreadsheet letters (0 → A, 25 → Z, 26 → AA).
fn col_letters(mut col: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

pub fn active_sheet_info() -> Result<SheetInfo> {
    let out = run_applescript(
        r#"tell application "Numbers"
  set d to front document
  set dn to name of d
  set sn to name of active sheet of d
  tell active sheet of d
    tell first table
      return dn & "|||" & sn & "|||" & (row count as string) & "|||" & (column count as string)
    end tell
  end tell
end tell"#,
    )?;
    let parts: Vec<&str> = out.split("|||").collect();
    let [doc_name, sheet_name, rows, cols] = parts.as_slice() else {
        bail!("unexpected response from Numbers: {out}");
    };
    Ok(SheetInfo {
        doc_name: doc_name.to_string(),
        sheet_name: sheet_name.to_string(),
        rows: rows.trim().parse().context("parsing row count")?,
        cols: cols.trim().parse().context("parsing column count")?,
    })
}

/// Reads every cell of the active sheet's first table, skipping formula
/// cells (never overwrite a computed value) and anything without letters.
pub fn read_translatable_cells(info: &SheetInfo) -> Result<Vec<SheetCell>> {
    if info.rows == 0 || info.cols == 0 {
        return Ok(Vec::new());
    }
    let range = format!("A1:{}{}", col_letters(info.cols - 1), info.rows);
    let script = format!(
        r#"tell application "Numbers"
  tell front document
    tell active sheet
      tell first table
        set vs to value of cells of range "{range}"
        set fs to formula of cells of range "{range}"
        set sep to (character id 2) & (character id 3)
        set out to ""
        repeat with i from 1 to (count of vs)
          set v to item i of vs
          set f to item i of fs
          if class of v is text and f is missing value then
            set out to out & i & tab & v & sep
          end if
        end repeat
        return out
      end tell
    end tell
  end tell
end tell"#
    );
    let raw = run_applescript(&script)?;
    let mut cells = Vec::new();
    for entry in raw.split("\u{2}\u{3}") {
        let entry = entry.trim_start_matches('\n');
        if entry.is_empty() {
            continue;
        }
        let Some((idx, text)) = entry.split_once('\t') else { continue };
        let Ok(idx) = idx.trim().parse::<usize>() else { continue };
        let text = text.trim_end_matches('\n').to_string();
        if !text.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        let i = idx - 1; // AppleScript is 1-indexed
        let row = i / info.cols;
        let col = i % info.cols;
        cells.push(SheetCell {
            cell_ref: format!("{}{}", col_letters(col), row + 1),
            text,
        });
    }
    Ok(cells)
}

fn escape_applescript_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\" & return & \"")
}

/// Row height (points) tall enough to show two stacked lines without
/// clipping, for typical spreadsheet font sizes.
const STACKED_ROW_HEIGHT: f64 = 30.0;

/// 1-indexed row number encoded in a cell reference like "AB12" → 12.
fn row_of_cell_ref(cell_ref: &str) -> Option<usize> {
    cell_ref.trim_start_matches(|c: char| c.is_ascii_alphabetic()).parse().ok()
}

/// Writes `original` stacked with `translation` (separated by a soft return)
/// back into each given cell, and grows any affected row that's still at
/// single-line height so the second line isn't clipped. One automation call.
pub fn write_stacked_cells(pairs: &[(String, String, String)]) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mut body = String::new();
    let mut rows: Vec<usize> = Vec::new();
    for (cell_ref, original, translation) in pairs {
        let stacked = format!(
            "{}\" & return & \"{}",
            escape_applescript_string(original),
            escape_applescript_string(translation)
        );
        body.push_str(&format!(
            "        set value of cell \"{cell_ref}\" to \"{stacked}\"\n"
        ));
        if let Some(row) = row_of_cell_ref(cell_ref) {
            if !rows.contains(&row) {
                rows.push(row);
            }
        }
    }
    let row_list = rows
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        r#"tell application "Numbers"
  tell front document
    tell active sheet
      tell first table
{body}        repeat with r in {{{row_list}}}
          set rn to r as integer
          if height of row rn < {STACKED_ROW_HEIGHT} then set height of row rn to {STACKED_ROW_HEIGHT}
        end repeat
      end tell
    end tell
  end tell
end tell"#
    );
    run_applescript(&script)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{col_letters, row_of_cell_ref};

    #[test]
    fn column_letters() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(5), "F");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(27), "AB");
    }

    #[test]
    fn row_extraction() {
        assert_eq!(row_of_cell_ref("A1"), Some(1));
        assert_eq!(row_of_cell_ref("F60"), Some(60));
        assert_eq!(row_of_cell_ref("AB123"), Some(123));
    }
}
