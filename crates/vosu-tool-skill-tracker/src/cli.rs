use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use std::io::{BufWriter, Write};
use vosu_tool_sdk::{
    ToolEnv,
    cli::{Method, Progress, dan, number, print_result, ratings, table, text},
    send_json,
};
#[derive(Parser, Debug)]
#[command(
    name = "vosux skill-tracker",
    about = "Inspect skill ratings and recorded plays"
)]
struct Args {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand, Debug)]
enum Command {
    Profile,
    History {
        #[arg(long,default_value="50",value_parser=clap::value_parser!(u32).range(1..=200))]
        last: u32,
        #[arg(long,value_parser=["osu","taiko","fruits","mania"])]
        mode: Option<String>,
    },
    Last,
    Export {
        #[arg(long, value_enum, default_value = "json")]
        format: Format,
    },
    Import,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Format {
    Json,
    Csv,
}
pub async fn run() -> Result<(), crate::Error> {
    let args = Args::parse_from(
        std::iter::once("vosux skill-tracker".to_owned()).chain(std::env::args().skip(2)),
    );
    let client = ToolEnv::from_env()?.client();
    let result: Value = match args.command {
        Command::Profile => client.own_json("v1/profile").await?,
        Command::History { last, mode } => {
            let mut path = format!("v1/history?limit={last}");
            if let Some(mode) = mode {
                path.push_str(&format!("&mode={mode}"));
            }
            client.own_json(&path).await?
        }
        Command::Last => {
            let history: Value = client.own_json("v1/history?limit=1").await?;
            history["plays"]
                .as_array()
                .and_then(|p| p.first())
                .cloned()
                .unwrap_or(Value::Null)
        }
        Command::Import => {
            let job: Value = send_json(client.own(Method::POST, "v1/import")).await?;
            let id = job["job"].as_str().ok_or("missing job id")?;
            let mut progress = Progress::new("importing stable scores");
            let result = client
                .wait_job("skill-tracker", id, |job| progress.update(job))
                .await;
            drop(progress);
            match result {
                Ok(job) => job,
                Err(e) => {
                    eprintln!("vosux skill-tracker: {e}");
                    std::process::exit(e.exit_code());
                }
            }
        }
        Command::Export { format } => {
            let mut output = BufWriter::new(std::io::stdout().lock());
            let mut before = None;
            let mut first = true;
            let columns = csv_columns();
            if matches!(format, Format::Json) {
                write!(output, "[")?;
            } else {
                writeln!(output, "{}", csv_header(&columns))?;
            }
            loop {
                let mut path = "v1/history?limit=200".to_owned();
                if let Some(before) = before {
                    path.push_str(&format!("&before={before}"));
                }
                let page: Value = client.own_json(&path).await?;
                let plays = page["plays"].as_array().ok_or("invalid history response")?;
                if plays.is_empty() {
                    break;
                }
                for play in plays {
                    match format {
                        Format::Json => {
                            if !first {
                                write!(output, ",")?;
                            }
                            write!(output, "{play}")?;
                        }
                        Format::Csv => {
                            // Fixed schema keeps empty and populated exports identical.
                            writeln!(output, "{}", csv_row(play, &columns))?;
                        }
                    }
                    first = false;
                }
                let next = page["before"].as_i64().ok_or("missing history cursor")?;
                if before.is_some_and(|old| next >= old) {
                    return Err("history cursor did not advance".into());
                }
                before = Some(next);
            }
            if matches!(format, Format::Json) {
                writeln!(output, "]")?;
            }
            output.flush()?;
            return Ok(());
        }
    };
    print_result(&result, args.json, || human(&result))?;
    Ok(())
}

fn csv_columns() -> Vec<String> {
    [
        "accuracy",
        "calculation_error",
        "checksum",
        "client",
        "difficulty",
        "eligible",
        "id",
        "judgments",
        "keys",
        "max_combo",
        "mode",
        "mods",
        "outcome",
        "pp",
        "rate",
        "score",
        "source",
        "stars",
        "timestamp",
        "title",
        "wife",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn csv_header(columns: &[String]) -> String {
    columns
        .iter()
        .map(|s| csv_cell(s))
        .collect::<Vec<_>>()
        .join(",")
}
fn human(value: &Value) -> String {
    if value["ratings"].is_object() {
        let counts = value["play_counts"]
            .as_object()
            .map(|counts| {
                counts
                    .iter()
                    .map(|(mode, n)| format!("{mode}: {n}"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_default();
        return format!(
            "Skill profile\n\n{}\n\n{}\nPlays: {}\nModel: {}\n{}",
            ratings(&value["ratings"], None),
            dan(&value["dan"], "no qualifying play"),
            if counts.is_empty() { "none" } else { &counts },
            text(&value["rating_model"]),
            text(&value["accuracy_notice"])
        );
    }
    if value["state"].is_string() {
        return format!(
            "Import {} — {}\n{}",
            text(&value["state"]),
            text(&value["message"]),
            value["result"]
        );
    }
    let plays: Vec<&Value> = if let Some(plays) = value["plays"].as_array() {
        plays.iter().collect()
    } else if value.is_null() {
        Vec::new()
    } else {
        vec![value]
    };
    let rows = plays
        .iter()
        .map(|play| {
            vec![
                text(&play["timestamp"]),
                text(&play["title"]),
                format!("{}×", number(&play["rate"])),
                format!("{}%", number(&play["accuracy"])),
                number(&play["difficulty"]["msd"]["overall"]),
                number(&play["difficulty"]["ssr"]["overall"]),
                text(&play["outcome"]),
                text(&play["wife"]["model"]),
            ]
        })
        .collect::<Vec<_>>();
    table(
        &[
            "Date",
            "Map",
            "Rate",
            "Acc",
            "MSD",
            "SSR",
            "Result",
            "Accuracy model",
        ],
        &rows,
    )
}
fn csv_cell(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn csv_row(play: &Value, columns: &[String]) -> String {
    columns
        .iter()
        .map(|key| {
            let v = &play[key];
            csv_cell(
                &v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments() {
        assert!(
            Args::try_parse_from([
                "skill-tracker",
                "history",
                "--last",
                "5",
                "--mode",
                "mania",
                "--json"
            ])
            .is_ok()
        );
        for a in [
            vec!["skill-tracker", "history", "--last", "0"],
            vec!["skill-tracker", "history", "--mode", "invalid"],
            vec!["skill-tracker", "export", "--format", "xml"],
        ] {
            assert!(Args::try_parse_from(a).is_err());
        }
    }
    #[test]
    fn csv_quotes() {
        assert_eq!(
            csv_row(
                &serde_json::json!({"title":"a,\"b\"\nnext","ssr":{"overall":12}}),
                &["title".into(), "ssr".into()]
            ),
            "\"a,\"\"b\"\"\nnext\",\"{\"\"overall\"\":12}\""
        );
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn empty_csv_has_full_schema_header() {
        let header = csv_header(&csv_columns());
        assert!(header.starts_with("\"accuracy\","));
        assert!(header.ends_with("\"wife\""));
        assert_eq!(csv_columns().len(), 21);
        assert!(header.contains("\"difficulty\""));
    }
    #[test]
    fn profile_and_play_tables_round_ratings() {
        let profile = human(
            &serde_json::json!({"ratings":{"overall":12.34567},"play_counts":{"mania":3},"dan":null,"rating_model":"test","accuracy_notice":"experimental"}),
        );
        assert!(profile.contains("12.35") && profile.contains("mania: 3"));
        assert!(profile.contains("dan: —"));
        let output = human(
            &serde_json::json!({"title":"Chart","timestamp":"2026-10-21","rate":0.85,"accuracy":98.23456,"difficulty":{"msd":{"overall":12.3456},"ssr":{"overall":11.2345}},"outcome":"passed","wife":{"model":"wife3-j4-offsets"}}),
        );
        assert!(output.contains("98.23%") && output.contains("12.35") && output.contains("11.23"));
        assert!(output.contains("wife3-j4-offsets") && output.contains("passed"));
        assert!(!output.contains("98.23456"));
    }
}
