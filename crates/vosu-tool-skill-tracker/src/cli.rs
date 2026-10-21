use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;
use std::io::{BufWriter, Write};
use vosu_tool_sdk::{
    ToolEnv,
    cli::{Method, print_output, progress},
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
            match client.wait_job("skill-tracker", id, progress).await {
                Ok(job) => job,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(e.exit_code());
                }
            }
        }
        Command::Export { format } => {
            let mut output = BufWriter::new(std::io::stdout().lock());
            let mut before = None;
            let mut first = true;
            let mut columns = Vec::new();
            if matches!(format, Format::Json) {
                write!(output, "[")?;
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
                            if first {
                                columns = play
                                    .as_object()
                                    .ok_or("invalid play")?
                                    .keys()
                                    .cloned()
                                    .collect();
                                writeln!(
                                    output,
                                    "{}",
                                    columns
                                        .iter()
                                        .map(|s| csv_cell(s))
                                        .collect::<Vec<_>>()
                                        .join(",")
                                )?;
                            }
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
    print_output(&result, args.json)?;
    Ok(())
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
