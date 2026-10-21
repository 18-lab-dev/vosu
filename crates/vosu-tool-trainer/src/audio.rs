use anyhow::{Context, Result, bail};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::{
    fs::File,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct Audio {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}
fn check(cancel: &AtomicBool) -> Result<()> {
    anyhow::ensure!(!cancel.load(Ordering::Relaxed), "cancelled");
    Ok(())
}
pub fn decode(path: &Path, cancel: &AtomicBool) -> Result<Audio> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let sample_rate;
    match ext.as_str() {
        "ogg" => {
            let mut d = vorbis_rs::VorbisDecoder::new(File::open(path)?)?;
            sample_rate = d.sampling_frequency().get();
            channels.resize(d.channels().get() as usize, Vec::new());
            while let Some(block) = d.decode_audio_block()? {
                check(cancel)?;
                for (out, input) in channels.iter_mut().zip(block.samples()) {
                    out.extend_from_slice(input);
                }
            }
        }
        "mp3" => {
            let mut d = minimp3::Decoder::new(File::open(path)?);
            let mut sr = None;
            loop {
                check(cancel)?;
                match d.next_frame() {
                    Ok(f) => {
                        if let Some(old) = sr {
                            anyhow::ensure!(
                                old == f.sample_rate && channels.len() == f.channels,
                                "MP3 format changed"
                            );
                        } else {
                            sr = Some(f.sample_rate);
                            channels.resize(f.channels, Vec::new());
                        }
                        for frame in f.data.chunks_exact(f.channels) {
                            for (out, s) in channels.iter_mut().zip(frame) {
                                out.push(*s as f32 / 32768.0);
                            }
                        }
                    }
                    Err(minimp3::Error::Eof) => break,
                    Err(minimp3::Error::SkippedData) => continue,
                    Err(e) => bail!("MP3 decode: {e:?}"),
                }
            }
            sample_rate = sr.context("empty MP3")? as u32;
        }
        "wav" => {
            let mut d = hound::WavReader::open(path)?;
            let spec = d.spec();
            sample_rate = spec.sample_rate;
            channels.resize(spec.channels as usize, Vec::new());
            if spec.sample_format == hound::SampleFormat::Float {
                for (i, v) in d.samples::<f32>().enumerate() {
                    channels[i % spec.channels as usize].push(v?);
                }
            } else {
                let divisor = 2_f32.powi(spec.bits_per_sample as i32 - 1);
                for (i, v) in d.samples::<i32>().enumerate() {
                    channels[i % spec.channels as usize].push(v? as f32 / divisor);
                }
            }
        }
        _ => bail!("supported audio: MP3, OGG Vorbis, WAV"),
    }
    anyhow::ensure!(
        !channels.is_empty() && channels.len() <= 2 && !channels[0].is_empty(),
        "audio must contain mono or stereo samples"
    );
    Ok(Audio {
        sample_rate,
        channels,
    })
}
pub fn change(
    audio: &Audio,
    rate: f64,
    preserve: bool,
    cancel: &AtomicBool,
    progress: impl Fn(f64),
) -> Result<Audio> {
    let n = audio.channels[0].len();
    let count = audio.channels.len();
    let target = (n as f64 / rate).round() as usize;
    let mut output = vec![Vec::with_capacity(target + 4096); count];
    if preserve {
        let mut stretch =
            signalsmith_stretch::Stretch::preset_default(count as u32, audio.sample_rate);
        // exact() compensates both input/output latency, unlike process(). Chunk the
        // outer job stages; Signalsmith's offline exact operation owns its pre-roll.
        let input: Vec<f32> = (0..n)
            .flat_map(|i| audio.channels.iter().map(move |c| c[i]))
            .collect();
        let mut interleaved = vec![0.0; target * count];
        check(cancel)?;
        anyhow::ensure!(
            stretch.exact(&input, &mut interleaved),
            "time-stretch failed"
        );
        check(cancel)?;
        for frame in interleaved.chunks_exact(count) {
            for (out, v) in output.iter_mut().zip(frame) {
                out.push(*v);
            }
        }
        progress(1.0);
    } else {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let mut r = SincFixedIn::<f32>::new(1.0 / rate, 1.0, params, 1024, count)?;
        let delay = r.output_delay();
        let mut pos = 0;
        let mut block = r.output_buffer_allocate(true);
        while output[0].len() < target + delay {
            check(cancel)?;
            let (_, written) = if pos + 1024 <= n {
                let input = [
                    &audio.channels[0][pos..pos + 1024],
                    &audio.channels[count - 1][pos..pos + 1024],
                ];
                r.process_into_buffer(&input[..count], &mut block, None)?
            } else {
                let mut input = [[0.0; 1024]; 2];
                if pos < n {
                    for (dest, src) in input.iter_mut().zip(&audio.channels) {
                        dest[..n - pos].copy_from_slice(&src[pos..]);
                    }
                }
                r.process_into_buffer(&input[..count], &mut block, None)?
            };
            for (out, v) in output.iter_mut().zip(&block) {
                out.extend_from_slice(&v[..written]);
            }
            pos += 1024;
            progress((pos as f64 / n as f64).min(1.0));
        }
        for out in &mut output {
            out.drain(..delay);
            out.truncate(target);
        }
    }
    Ok(Audio {
        sample_rate: audio.sample_rate,
        channels: output,
    })
}
pub fn encode(audio: &Audio, path: &Path, cancel: &AtomicBool) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let result = (|| {
        let mut e = vorbis_rs::VorbisEncoderBuilder::new(
            audio.sample_rate.try_into()?,
            (audio.channels.len() as u8).try_into()?,
            file,
        )?
        .build()?;
        for pos in (0..audio.channels[0].len()).step_by(4096) {
            check(cancel)?;
            let end = (pos + 4096).min(audio.channels[0].len());
            let count = audio.channels.len();
            let block = [
                &audio.channels[0][pos..end],
                &audio.channels[count - 1][pos..end],
            ];
            e.encode_audio_block(&block[..count])?;
        }
        e.finish()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}
