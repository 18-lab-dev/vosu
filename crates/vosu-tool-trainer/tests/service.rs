use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::Duration,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn isolated_service_generates_stretch_and_resample() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::env::var_os("TRAINER_SMOKE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temp.path().to_owned());
    fs::create_dir_all(&root).unwrap();
    let songs = root.join("Songs");
    let folder = songs.join("Synthetic — trainer smoke");
    fs::create_dir_all(&folder).unwrap();
    let original = include_str!("fixtures/all.osu");
    fs::write(folder.join("original.osu"), original).unwrap();
    let mut wav = hound::WavWriter::create(
        folder.join("tone.wav"),
        hound::WavSpec {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for i in 0..88200 {
        wav.write_sample(
            ((i as f32 * 440.0 * std::f32::consts::TAU / 44100.0).sin() * 10000.0) as i16,
        )
        .unwrap();
    }
    wav.finalize().unwrap();
    let ctx = json!({"client":"stable","songs_dir":songs,"beatmap":{"checksum":"synthetic","mode":"mania","folder":folder,"file":folder.join("original.osu"),"audio":folder.join("tone.wav"),"background":null}});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/tools/_host/v1/context",
                get(move || {
                    let c = ctx.clone();
                    async move { Json(c) }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_vosu-trainer"));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .env("VOSU_TOOL_PROTOCOL", "1")
        .env("VOSU_TOOL_ID", "trainer")
        .env("VOSU_TOOL_TOKEN", "smoke-secret")
        .env("VOSU_URL", format!("http://{address}"))
        .env("VOSU_TOOL_DATA_DIR", root.join("data"))
        .env("VOSU_TOOL_CACHE_DIR", root.join("cache"))
        .env("HOME", &root);
    for name in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        command.env(name, p);
    }
    let mut child = command.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port: u16 = line
        .trim()
        .strip_prefix("vosu-tool-ready ")
        .unwrap()
        .parse()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    for (rate, preserve) in [(1.3, true), (0.8, false)] {
        let response = client
            .post(format!("{base}/v1/generate"))
            .header("x-vosu-tool-token", "smoke-secret")
            .json(&json!({"rate":rate,"pitch_preserving":preserve}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let job: Value = response.json().await.unwrap();
        let final_job = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let v: Value = client
                    .get(format!("{base}/v1/jobs/{}", job["job"].as_str().unwrap()))
                    .header("x-vosu-tool-token", "smoke-secret")
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if v["state"] == "done" || v["state"] == "failed" {
                    break v;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(final_job["state"], "done", "{final_job}");
        let files = final_job["result"]["files"].as_array().unwrap();
        let osu = files[0]["path"].as_str().unwrap();
        let audio = files[1]["path"].as_str().unwrap();
        let output = fs::read_to_string(osu).unwrap();
        println!(
            "\n{rate}x {} → {osu}",
            if preserve { "stretch" } else { "resample" }
        );
        for line in output.lines().filter(|l| {
            l.starts_with("Version:")
                || l.starts_with("AudioFilename:")
                || l.starts_with("BeatmapID:")
                || l.starts_with("128,192,")
                || l.starts_with("448,192,")
        }) {
            println!("+ {line}");
        }
        let mut decoder = vorbis_rs::VorbisDecoder::new(fs::File::open(audio).unwrap()).unwrap();
        let sr = decoder.sampling_frequency().get();
        let mut samples = 0;
        while let Some(block) = decoder.decode_audio_block().unwrap() {
            samples += block.samples()[0].len();
        }
        let seconds = samples as f64 / sr as f64;
        println!("OGG duration: {seconds:.6}s (source: 2.000000s)");
        assert!((seconds - 2.0 / rate).abs() < 0.01);
        assert_eq!(
            fs::read_to_string(folder.join("original.osu")).unwrap(),
            original
        );
    }
    drop(child.stdin.take());
    let status = tokio::task::spawn_blocking(move || child.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success());
    host.abort();
}
