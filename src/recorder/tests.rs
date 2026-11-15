//! Hardware integration tests (need a desktop session, a GPU and an audio output).
//! Run with: `cargo test recorder -- --ignored --nocapture --test-threads=1`

use super::*;
use crate::settings::recorder::{EncoderPreference, RecorderSettings, VideoFormat};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// The recorder is a process-wide singleton: run these one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn out_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("snapforge-rec-tests-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn settings(format: VideoFormat, fps: u32) -> RecorderSettings {
    RecorderSettings {
        format,
        fps,
        encoder: match std::env::var("SF_ENC").as_deref() {
            Ok("sw") => EncoderPreference::Software,
            Ok("hw") => EncoderPreference::Hardware,
            _ => EncoderPreference::Auto,
        },
        record_system_audio: std::env::var_os("SF_NOAUDIO").is_none(),
        record_microphone: false,
        show_cursor: true,
        highlight_clicks: true,
        ..Default::default()
    }
}

fn cpu_time_100ns() -> u64 {
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid out pointers for the current process.
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    f(k) + f(u)
}

/// Record `secs` seconds; returns the result and the process CPU time used meanwhile.
fn record(area: Option<Area>, s: RecorderSettings, secs: f32, path: &Path, mid: impl FnOnce()) -> (Finished, f64) {
    let _ = std::fs::remove_file(path);
    let cpu0 = cpu_time_100ns();
    let t0 = Instant::now();
    start_with(
        area,
        StartOptions { out_path: Some(path.to_path_buf()), headless: true, settings: Some(s), ..Default::default() },
    )
    .expect("start");
    std::thread::sleep(Duration::from_secs_f32(secs / 2.0));
    mid();
    std::thread::sleep(Duration::from_secs_f32(secs / 2.0));
    let f = stop_blocking().expect("stop");
    let wall = t0.elapsed().as_secs_f64();
    let cpu = (cpu_time_100ns() - cpu0) as f64 / 1e7;
    (f, cpu / wall)
}

/// RMS of the decoded audio between 2 s and 8 s (checks the tone really was captured).
fn audio_rms(path: &Path) -> f32 {
    let _com = mf::ComGuard::mta();
    let Ok(reader) = mf::source_reader(path, None) else { return 0.0 };
    let mut sum = 0f64;
    let mut n = 0u64;
    // SAFETY: media type setup and synchronous reads with valid out pointers.
    unsafe {
        let first = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        let _ = reader.SetStreamSelection(first, true);
        let Ok(t) = MFCreateMediaType() else { return 0.0 };
        let _ = t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio);
        let _ = t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float);
        if reader.SetCurrentMediaType(first, None, &t).is_err() {
            return 0.0;
        }
        loop {
            let (mut flags, mut ts) = (0u32, 0i64);
            let mut s: Option<IMFSample> = None;
            if reader.ReadSample(first, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut s)).is_err() {
                break;
            }
            if let Some(s) = s {
                if (2 * clock::HNS_PER_SEC..8 * clock::HNS_PER_SEC).contains(&ts) {
                    if let Ok(b) = s.ConvertToContiguousBuffer() {
                        let mut p = std::ptr::null_mut();
                        let mut len = 0u32;
                        if b.Lock(&mut p, None, Some(&mut len)).is_ok() {
                            let f = std::slice::from_raw_parts(p as *const f32, len as usize / 4);
                            sum += f.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>();
                            n += f.len() as u64;
                            let _ = b.Unlock();
                        }
                    }
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
    }
    if n == 0 {
        0.0
    } else {
        (sum / n as f64).sqrt() as f32
    }
}

fn ms(hns: i64) -> f64 {
    hns as f64 / 10_000.0
}

#[test]
#[ignore = "needs a desktop session, GPU and audio output"]
fn record_10s_h264_aac_av_sync() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::logging::init(&std::env::var("SF_LOG").unwrap_or_else(|_| "info".into()));
    let path = out_dir().join("sync10.mp4");
    let stop_tone = Arc::new(AtomicBool::new(false));
    let tone = wasapi::play_tone(440.0, 12.0, stop_tone.clone());
    std::thread::sleep(Duration::from_millis(300));
    let (f, cpu) = record(None, settings(VideoFormat::Mp4, 30), 10.0, &path, || {});
    stop_tone.store(true, Ordering::Release);
    let tone_ok = tone.join().map(|r| r.is_ok()).unwrap_or(false);
    let info = mf::probe(&f.path).expect("probe");
    let v = info.video().expect("video stream");
    let a = info.audio().expect("audio stream");
    let rms = audio_rms(&f.path);
    println!(
        "10 s test: file {} | backend {} | hw encoder {} | frames {} dropped {} duplicates {} | cpu {:.1}% of one core",
        f.path.display(),
        f.backend,
        f.hardware_encoder,
        f.frames,
        f.dropped,
        f.duplicated,
        cpu * 100.0
    );
    println!(
        "  container duration {:.1} ms | video {} {}x{} {} samples {:.1}..{:.1} ms | audio {} {} Hz {} samples {:.1}..{:.1} ms",
        ms(info.duration),
        mf::subtype_name(&v.subtype),
        v.width,
        v.height,
        v.samples,
        ms(v.first_ts),
        ms(v.end_ts),
        mf::subtype_name(&a.subtype),
        a.rate,
        a.samples,
        ms(a.first_ts),
        ms(a.end_ts)
    );
    let start_drift = (v.first_ts - a.first_ts).abs();
    let end_drift = (v.end_ts - a.end_ts).abs();
    println!(
        "  A/V drift: start {:.1} ms, end {:.1} ms | tone played: {tone_ok} | captured RMS {rms:.4}",
        ms(start_drift),
        ms(end_drift)
    );
    assert_eq!(v.subtype, MFVideoFormat_H264);
    assert_eq!(a.subtype, MFAudioFormat_AAC);
    assert!((ms(v.end_ts) - 10_000.0).abs() < 400.0, "video duration {:.1} ms", ms(v.end_ts));
    assert!((ms(a.end_ts) - 10_000.0).abs() < 400.0, "audio duration {:.1} ms", ms(a.end_ts));
    assert!(ms(start_drift) < 40.0 && ms(end_drift) < 40.0, "A/V drift too large");
    if tone_ok {
        assert!(rms > 0.01, "loopback tone not captured (rms {rms})");
    }
}

#[test]
#[ignore = "needs a desktop session and a GPU"]
fn measure_1080p60_cpu_and_drops() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::logging::init(&std::env::var("SF_LOG").unwrap_or_else(|_| "info".into()));
    let m = win::monitors().into_iter().find(|m| m.primary).expect("primary monitor");
    let r = Rect::new(m.rect.x, m.rect.y, 1920.min(m.rect.w), 1080.min(m.rect.h));
    let mut s = settings(VideoFormat::Mp4, 60);
    s.encoder = EncoderPreference::Hardware;
    let path = out_dir().join("p1080_60.mp4");
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    let (f, cpu) = record(Some(Area::Rect(r)), s, 10.0, &path, || {});
    let info = mf::probe(&f.path).expect("probe");
    let v = info.video().expect("video");
    println!(
        "1080p60: {}x{} backend {} hw encoder {} | frames {} dropped {} duplicates {} | CPU {:.1}% of one core = {:.2}% of {} logical CPUs | {} samples in file, {:.0} ms",
        v.width,
        v.height,
        f.backend,
        f.hardware_encoder,
        f.frames,
        f.dropped,
        f.duplicated,
        cpu * 100.0,
        cpu * 100.0 / cores,
        cores,
        v.samples,
        ms(v.end_ts)
    );
    assert!(v.samples >= 540, "too few frames: {}", v.samples);
}

#[test]
#[ignore = "needs a desktop session and a GPU"]
fn crash_recovery_from_partial_fragmented_mp4() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::logging::init(&std::env::var("SF_LOG").unwrap_or_else(|_| "info".into()));
    let dir = out_dir().join("crash");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("live.mp4");
    let snapshot = dir.join("crashed.recording.mp4");
    let mut s = settings(VideoFormat::Mp4, 30);
    s.keyframe_interval_secs = 1;
    let partial = journal::partial_for(&path);
    let (snap, sp) = (snapshot.clone(), partial.clone());
    // Copy the file mid-recording: exactly what a power loss leaves on disk.
    let (f, _) = record(None, s, 6.0, &path, move || {
        std::thread::sleep(Duration::from_millis(1500));
        std::fs::copy(&sp, &snap).expect("snapshot");
    });
    assert!(f.path.exists());
    let snap_len = std::fs::metadata(&snapshot).map(|m| m.len()).unwrap_or(0);
    let complete = journal::fmp4_complete_len_of(&snapshot).expect("scan");
    println!("crash snapshot: {snap_len} bytes, playable prefix {complete:?}");
    let j = journal::Journal {
        partial: snapshot.clone(),
        final_path: dir.join("crashed.mp4"),
        format: "mp4".into(),
        started: String::new(),
        pid: 1,
    };
    let jdir = dir.join("journal");
    j.write(&jdir).expect("journal");
    let recovered = recover_pending_in(&jdir);
    assert_eq!(recovered.len(), 1, "nothing recovered");
    let info = mf::probe(&recovered[0]).expect("probe recovered");
    let v = info.video().expect("video");
    println!(
        "recovered {} : {:.0} ms video ({} frames), {:.0} ms audio",
        recovered[0].display(),
        ms(v.end_ts),
        v.samples,
        info.audio().map(|a| ms(a.end_ts)).unwrap_or(0.0)
    );
    assert!(ms(v.end_ts) > 2000.0, "recovered too little");
    assert!(journal::scan_pending(&jdir, 0).is_empty(), "journal not removed");
}

#[test]
#[ignore = "needs a desktop session and a GPU"]
fn wmv_and_conversions() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::logging::init(&std::env::var("SF_LOG").unwrap_or_else(|_| "info".into()));
    let dir = out_dir();
    let wmv = dir.join("clip.wmv");
    let (f, _) = record(None, settings(VideoFormat::Wmv, 15), 3.0, &wmv, || {});
    let info = mf::probe(&f.path).expect("probe wmv");
    println!(
        "wmv: video {:?} {:.0} ms, audio {:?}",
        info.video().map(|v| mf::subtype_name(&v.subtype)),
        info.video().map(|v| ms(v.end_ts)).unwrap_or(0.0),
        info.audio().map(|a| mf::subtype_name(&a.subtype))
    );
    assert_eq!(info.video().map(|v| v.subtype), Some(MFVideoFormat_WMV3));

    let mp4 = dir.join("from_wmv.mp4");
    let t = Instant::now();
    convert_to_mp4(&f.path, &mp4, &ConvertOptions::default(), &mut |_| true).expect("convert");
    let ci = mf::probe(&mp4).expect("probe converted");
    println!(
        "convert wmv->mp4 in {:.0} ms: {:?} {:.0} ms / {:?}",
        t.elapsed().as_secs_f64() * 1000.0,
        ci.video().map(|v| mf::subtype_name(&v.subtype)),
        ci.video().map(|v| ms(v.end_ts)).unwrap_or(0.0),
        ci.audio().map(|a| mf::subtype_name(&a.subtype))
    );
    assert_eq!(ci.video().map(|v| v.subtype), Some(MFVideoFormat_H264));

    for (fmt, ext, sub) in [(AudioFormat::M4a, "m4a", MFAudioFormat_AAC), (AudioFormat::Mp3, "mp3", MFAudioFormat_MP3)]
    {
        let out = dir.join(format!("audio.{ext}"));
        extract_audio(&mp4, &out, fmt, 192, &mut |_| true).expect("extract");
        let ai = mf::probe(&out).expect("probe audio");
        let a = ai.audio().expect("audio stream");
        println!("extract {ext}: {} {:.0} ms", mf::subtype_name(&a.subtype), ms(a.end_ts));
        assert_eq!(a.subtype, sub);
    }

    let mp3 = dir.join("audio_only.mp3");
    let (f, _) = record(None, settings(VideoFormat::Mp3AudioOnly, 30), 2.0, &mp3, || {});
    let ai = mf::probe(&f.path).expect("probe mp3");
    println!(
        "mp3 recording: {:?} {:.0} ms",
        ai.audio().map(|a| mf::subtype_name(&a.subtype)),
        ai.audio().map(|a| ms(a.end_ts)).unwrap_or(0.0)
    );
    assert_eq!(ai.audio().map(|a| a.subtype), Some(MFAudioFormat_MP3));
}

#[test]
#[ignore = "needs a desktop session and a GPU"]
fn pause_shifts_timestamps() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    crate::logging::init(&std::env::var("SF_LOG").unwrap_or_else(|_| "info".into()));
    // 4 s wall time with a 2 s pause in the middle -> about 2 s of media.
    let path2 = out_dir().join("pause2.mp4");
    let _ = std::fs::remove_file(&path2);
    start_with(
        None,
        StartOptions {
            out_path: Some(path2.clone()),
            headless: true,
            settings: Some(settings(VideoFormat::Mp4, 30)),
            ..Default::default()
        },
    )
    .expect("start");
    std::thread::sleep(Duration::from_secs(1));
    toggle_pause();
    std::thread::sleep(Duration::from_secs(2));
    toggle_pause();
    std::thread::sleep(Duration::from_secs(1));
    let f = stop_blocking().expect("stop");
    let info = mf::probe(&f.path).expect("probe");
    let v = info.video().expect("video");
    let a = info.audio().expect("audio");
    println!("pause test: video {:.0} ms, audio {:.0} ms", ms(v.end_ts), ms(a.end_ts));
    assert!((ms(v.end_ts) - 2000.0).abs() < 300.0);
    assert!((ms(v.end_ts) - ms(a.end_ts)).abs() < 40.0);
}

/// Debug helper: `SF_DUMP=<file> cargo test dump_samples -- --ignored --nocapture`.
#[test]
#[ignore = "debug helper"]
fn dump_samples() {
    let Ok(p) = std::env::var("SF_DUMP") else { return };
    let _com = mf::ComGuard::mta();
    let reader = mf::source_reader(Path::new(&p), None).expect("open");
    let streams = mf::native_streams(&reader);
    let mut n = 0;
    loop {
        let (mut idx, mut flags, mut ts) = (0u32, 0u32, 0i64);
        let mut s: Option<IMFSample> = None;
        // SAFETY: valid out pointers.
        let r = unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut idx),
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut s),
            )
        };
        if r.is_err() {
            break;
        }
        if let Some(s) = s {
            // SAFETY: plain getters.
            let (d, len) = unsafe { (s.GetSampleDuration().unwrap_or(-1), s.GetTotalLength().unwrap_or(0)) };
            let audio = streams.iter().find(|x| x.0 == idx).map(|x| x.2).unwrap_or(false);
            n += 1;
            println!("{}#{n} ts {:.1} dur {:.1} len {len}", if audio { "a" } else { "v" }, ms(ts), ms(d));
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            // SAFETY: deselect the finished stream.
            unsafe {
                let _ = reader.SetStreamSelection(idx, false);
            }
            // SAFETY: plain query.
            if streams
                .iter()
                .all(|(i, _, _)| !unsafe { reader.GetStreamSelection(*i) }.map(|b| b.as_bool()).unwrap_or(false))
            {
                break;
            }
        }
    }
}
