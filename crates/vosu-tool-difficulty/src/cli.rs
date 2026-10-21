use clap::Parser;
use serde_json::Value;
use std::io::Read;
use vosu_tool_sdk::{
    ToolEnv,
    cli::{Method, SKILLS, dan, number, print_result, range, ratings, table, text},
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
    let result: Value = if let Some(file) = &args.file {
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
    let header = format!(
        "{} — {}K · {} notes · {}×",
        args.file.as_deref().unwrap_or("Selected beatmap"),
        result["keys"],
        result["notes"],
        number(&result["rate"])
    );
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
    print_result(&result, args.json, || human(&result, &header))?;
    Ok(())
}

fn human(result: &Value, header: &str) -> String {
    if let Some(rates) = result["rates"].as_array() {
        let rows = rates
            .iter()
            .map(|r| {
                std::iter::once(format!("{}×", number(&r["rate"])))
                    .chain(SKILLS.iter().map(|s| number(&r["msd"][s])))
                    .collect()
            })
            .collect::<Vec<_>>();
        let headers = std::iter::once("Rate").chain(SKILLS).collect::<Vec<_>>();
        return format!("{header}\n\n{}", table(&headers, &rows));
    }
    let reason = result["unsupported"]
        .as_str()
        .unwrap_or("no estimate for this key count or rating");
    let unsupported = if result["unsupported"].is_string() {
        format!("\nUnsupported: {}", text(&result["unsupported"]))
    } else {
        String::new()
    };
    format!(
        "{header}\n\n{}\n\n{}{}",
        ratings(&result["msd"], Some(&result["ssr"])),
        dan(&result["dan"], reason),
        unsupported
    )
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

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn terminal_tables_round_values_and_explain_missing_dan() {
        let value =
            serde_json::json!({"msd":{"overall":12.345678},"ssr":{"overall":11.123456},"dan":null});
        let output = human(&value, "chart.osu — 4K · 100 notes · 0.85×");
        assert!(output.contains("chart.osu"));
        assert!(output.contains("12.35") && output.contains("11.12"));
        assert!(output.contains("dan: — (no estimate"));
        assert!(!output.contains("12.345678"));
        let output = human(
            &serde_json::json!({"rates":[{"rate":0.7,"msd":{"overall":6.721776}}]}),
            "Rate table",
        );
        assert!(output.contains("0.70×") && output.contains("6.72"));
        assert!(!output.contains("rates["));
    }
}
