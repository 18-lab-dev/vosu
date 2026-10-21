use clap::Parser;
use serde_json::Value;
use std::io::Read;
use vosu_tool_sdk::{
    ToolEnv,
    cli::{Method, print_output, range},
    send_json,
};
#[derive(Parser, Debug)]
#[command(
    name = "vosux difficulty",
    about = "Calculate the selected beatmap, a .osu file, or stdin (-)"
)]
struct Args {
    file: Option<String>,
    #[arg(long, default_value="1", value_parser=rate)]
    rate: f64,
    #[arg(long, value_parser=score)]
    score: Option<f64>,
    #[arg(long)]
    rates: bool,
    #[arg(long)]
    json: bool,
}
fn rate(s: &str) -> Result<f64, String> {
    range(s, 0.5, 2.0)
}
fn score(s: &str) -> Result<f64, String> {
    range(s, 0.0, 1.0)
}
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse_from(
        std::iter::once("vosux difficulty".to_owned()).chain(std::env::args().skip(2)),
    );
    let client = ToolEnv::from_env()?.client();
    let mut query = format!("rate={}", args.rate);
    if let Some(score) = args.score {
        query.push_str(&format!("&score={score}"));
    }
    let result: Value = if let Some(file) = args.file {
        let reader: Box<dyn Read> = if file == "-" {
            Box::new(std::io::stdin())
        } else {
            Box::new(std::fs::File::open(file)?)
        };
        let mut bytes = Vec::new();
        reader
            .take((vosu_tool_difficulty::MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > vosu_tool_difficulty::MAX_BYTES {
            return Err("beatmap exceeds 1 MiB".into());
        }
        send_json(
            client
                .own(Method::POST, &format!("v1/calculate?{query}"))
                .body(bytes),
        )
        .await?
    } else {
        client
            .own_json(&format!("v1/beatmap/current?{query}"))
            .await?
    };
    let result = if args.rates {
        let checksum = result["checksum"]
            .as_str()
            .ok_or("missing beatmap checksum")?;
        client
            .own_json::<Value>(&format!("v1/beatmap/{checksum}/rates"))
            .await?
    } else {
        result
    };
    print_output(&result, args.json)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments() {
        assert_eq!(
            Args::try_parse_from(["difficulty", "-", "--rate", "0.85"])
                .unwrap()
                .file
                .as_deref(),
            Some("-")
        );
        for args in [
            vec!["difficulty", "--rate", "0.4"],
            vec!["difficulty", "--score", "1.01"],
            vec!["difficulty", "--rate", "NaN"],
        ] {
            assert!(Args::try_parse_from(args).is_err());
        }
        assert!(Args::try_parse_from(["difficulty", "--rates", "--json"]).is_ok());
    }
}
