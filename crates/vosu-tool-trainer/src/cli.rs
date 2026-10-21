use clap::{Parser, Subcommand};
use serde_json::Value;
use vosu_tool_sdk::{
    ToolEnv,
    cli::{CliError, Method, Progress, print_result, range, table, text},
    send_json,
};
#[derive(Parser, Debug)]
#[command(
    name = "vosux trainer",
    about = "Generate practice copies of the selected beatmap",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, conflicts_with="bpm", value_parser=rate)]
    rate: Option<f64>,
    #[arg(long, value_parser=bpm)]
    bpm: Option<f64>,
    #[arg(long, conflicts_with = "no_pitch")]
    pitch: bool,
    #[arg(long)]
    no_pitch: bool,
    #[arg(long,value_parser=stat)]
    hp: Option<f64>,
    #[arg(long,value_parser=stat)]
    cs: Option<f64>,
    #[arg(long,value_parser=stat)]
    ar: Option<f64>,
    #[arg(long,value_parser=stat)]
    od: Option<f64>,
    #[arg(long)]
    lock_ar: bool,
    #[arg(long)]
    lock_od: bool,
    #[arg(long, global = true)]
    json: bool,
}
#[derive(Subcommand, Debug)]
enum Command {
    List,
    Delete { id: String },
}
fn rate(s: &str) -> Result<f64, String> {
    range(s, 0.5, 2.)
}
fn stat(s: &str) -> Result<f64, String> {
    range(s, 0., 10.)
}
fn bpm(s: &str) -> Result<f64, String> {
    let n = range(s, f64::MIN_POSITIVE, f64::MAX)?;
    Ok(n)
}
pub async fn run() -> anyhow::Result<()> {
    let args = Args::parse_from(
        std::iter::once("vosux trainer".to_owned()).chain(std::env::args().skip(2)),
    );
    let client = ToolEnv::from_env()?.client();
    let result: Value = match args.command {
        Some(Command::List) => client.own_json("v1/generated").await?,
        Some(Command::Delete { id }) => {
            send_json(client.own(Method::POST, &format!("v1/delete/{id}"))).await?
        }
        None => {
            let options = crate::map::Options {
                rate: args.rate.unwrap_or(1.),
                target_bpm: args.bpm,
                pitch_preserving: !args.no_pitch,
                hp: args.hp,
                cs: args.cs,
                ar: args.ar,
                od: args.od,
                lock_ar: args.lock_ar,
                lock_od: args.lock_od,
            };
            let job: Value =
                send_json(client.own(Method::POST, "v1/generate").json(&options)).await?;
            let id = job["job"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing job id"))?;
            let label = match args.bpm {
                Some(bpm) => format!("generating at {bpm:.2} BPM"),
                None => format!("generating {:.2}×", options.rate),
            };
            let mut progress = Progress::new(label);
            let job = client
                .wait_job("trainer", id, |job| progress.update(job))
                .await;
            drop(progress);
            let job = match job {
                Ok(job) => job,
                Err(e) => {
                    eprintln!("vosux trainer: {e}");
                    std::process::exit(e.exit_code());
                }
            };
            if let Some(warnings) = job["result"]["warnings"].as_array() {
                for warning in warnings {
                    if !args.json {
                        println!("Warning: {}", text(warning));
                    }
                }
            }
            if let Some(warning) = job["import_error"].as_str()
                && !args.json
            {
                println!("Warning: {warning}");
            }
            if !args.json {
                println!(
                    "{}",
                    created_path(&job["result"]).ok_or_else(|| CliError::Failed(
                        "service returned no practice file".into()
                    ))?
                );
                return Ok(());
            }
            job["result"].clone()
        }
    };
    print_result(&result, args.json, || human(&result))?;
    Ok(())
}
fn created_path(result: &Value) -> Option<&str> {
    result["files"]
        .as_array()?
        .iter()
        .filter_map(|f| f["path"].as_str())
        .find(|p| p.ends_with(".osu") || p.ends_with(".osz"))
}

fn human(result: &Value) -> String {
    if let Some(id) = result["deleted"].as_str() {
        return format!("Deleted {id}");
    }
    let rows = result["maps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            vec![
                text(&entry["id"]),
                text(&entry["name"]),
                created_path(entry).unwrap_or("—").to_owned(),
            ]
        })
        .collect::<Vec<_>>();
    table(&["ID", "Practice copy", "Path"], &rows)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments() {
        for a in [
            vec!["trainer", "--rate", "1", "--bpm", "120"],
            vec!["trainer", "--rate", "2.1"],
            vec!["trainer", "--hp", "11"],
            vec!["trainer", "--bpm", "0"],
            vec!["trainer", "--pitch", "--no-pitch"],
        ] {
            assert!(Args::try_parse_from(a).is_err());
        }
        assert!(Args::try_parse_from(["trainer", "--rate", "0.85", "--json"]).is_ok());
        assert!(Args::try_parse_from(["trainer", "list", "--json"]).is_ok());
    }
    #[test]
    fn output() {
        assert_eq!(
            created_path(
                &serde_json::json!({"files":[{"path":"audio.ogg"},{"path":"practice.osu"}]})
            ),
            Some("practice.osu")
        );
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn list_and_delete_are_concise() {
        let output = human(
            &serde_json::json!({"maps":[{"id":"abc","name":"Practice","files":[{"path":"/Songs/practice.osu"}]}]}),
        );
        assert!(output.contains("Practice copy") && output.contains("/Songs/practice.osu"));
        assert!(!output.contains("maps["));
        assert_eq!(human(&serde_json::json!({"deleted":"abc"})), "Deleted abc");
    }
}
