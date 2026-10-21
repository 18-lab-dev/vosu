use rosu_map::Beatmap;
use vosu_tool_difficulty::{Params, calculate, checksum, estimate_dan, rows};
fn chart(mode: u8, keys: u8, objects: &str) -> Vec<u8> {
    format!("osu file format v14\n[General]\nMode:{mode}\n[Difficulty]\nCircleSize:{keys}\n[HitObjects]\n{objects}").into_bytes()
}
#[test]
fn four_key_rows_merge_and_hold_heads_only() {
    let b = chart(
        3,
        4,
        "0,192,1000,1,0\n127,192,1000,1,0\n128,192,1000,1,0\n256,192,2000,128,0,4000:0:0:0:0:\n512,192,3000,1,0\n",
    );
    let r = rows(&Beatmap::from_bytes(&b).unwrap(), 4).unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].notes, 3);
    assert_eq!(r[1].notes, 4);
    assert_eq!(r[2].notes, 8);
    assert_eq!(r[1].seconds, 2.0);
}
#[test]
fn seven_key_column_mapping() {
    let b = chart(
        3,
        7,
        "0,192,1000,1,0\n73,192,1100,1,0\n74,192,1200,1,0\n256,192,1300,1,0\n511,192,1400,1,0\n",
    );
    let r = rows(&Beatmap::from_bytes(&b).unwrap(), 7).unwrap();
    assert_eq!(
        r.iter().map(|r| r.notes).collect::<Vec<_>>(),
        [1, 1, 2, 8, 64]
    );
}
#[test]
fn rate_validation_and_keying() {
    for r in [f32::NAN, f32::INFINITY, 0.49, 2.01] {
        assert!(Params::new(r, None).is_err());
    }
    for s in [f32::NAN, -0.01, 1.01] {
        assert!(Params::new(1.0, Some(s)).is_err());
    }
    let p = Params::new(1.024, None).unwrap();
    assert_eq!(p.rate(), 1.0);
    assert_eq!(Params::new(1.026, None).unwrap().rate(), 1.05);
    assert_eq!(p.key("a"), Params::new(1.0, None).unwrap().key("a"));
    assert_ne!(p.key("a"), p.key("b"));
    assert_ne!(p.key("a"), Params::new(1.05, None).unwrap().key("a"));
    assert_ne!(p.key("a"), Params::new(1.0, Some(0.93)).unwrap().key("a"));
    assert_ne!(
        Params::new(1.0, Some(0.94)).unwrap().key("a"),
        Params::new(1.0, Some(0.93)).unwrap().key("a")
    );
    assert_eq!(checksum(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
}
#[test]
fn unsupported_modes_and_keys() {
    for (mode, keys) in [(0, 4), (1, 4), (2, 4), (3, 5), (3, 8)] {
        let d = calculate(&chart(mode, keys, ""), Params::new(1.0, None).unwrap()).unwrap();
        assert!(d.unsupported.is_some());
        assert!(d.msd.is_none() && d.ssr.is_none() && d.dan.is_none());
    }
    for keys in [4, 6, 7] {
        assert!(
            calculate(&chart(3, keys, ""), Params::new(1.0, None).unwrap())
                .unwrap()
                .unsupported
                .is_none()
        );
    }
}
#[test]
fn reference_golden_and_ssr() {
    let d = calculate(
        include_bytes!("fixtures/cyclic.osu"),
        Params::new(1.0, Some(0.93)).unwrap(),
    )
    .unwrap();
    assert!((d.msd.unwrap().overall - 6.11).abs() < 0.0001);
    assert!(d.ssr.unwrap().overall.is_finite());
    assert_eq!(d.notes, 1200);
}
#[test]
fn dan_interpolation_is_calibrated_and_not_extrapolated() {
    assert_eq!(estimate_dan(15.5289).unwrap().value, 1.0);
    assert_eq!(estimate_dan(26.4069).unwrap().value, 10.0);
    assert!((estimate_dan((15.5289 + 17.0718) / 2.0).unwrap().value - 1.5).abs() < 0.00001);
    assert!(estimate_dan(0.0).is_none());
    assert!(estimate_dan(40.0).is_none());
}

#[test]
fn nonempty_six_and_seven_key_calculation_and_score_endpoints() {
    for keys in [4, 6, 7] {
        let source = include_str!("fixtures/cyclic.osu")
            .replace("CircleSize:4", &format!("CircleSize:{keys}"));
        for score in [0.0, 0.93, 1.0] {
            let result =
                calculate(source.as_bytes(), Params::new(1.0, Some(score)).unwrap()).unwrap();
            assert!(result.msd.unwrap().overall > 0.0);
            assert!(result.ssr.unwrap().overall.is_finite());
            if keys != 4 {
                assert!(result.dan.is_none());
            }
        }
    }
}
