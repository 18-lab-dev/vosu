use crate::rating;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

pub const FIELDS: [&str; 8] = [
    "/state",
    "/client",
    "/beatmap",
    "/play",
    "/resultsScreen",
    "/game/paused",
    "/session/playCount",
    "/rankedPlay",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Play {
    pub id: i64,
    pub checksum: String,
    pub title: String,
    pub mode: String,
    pub keys: u32,
    pub client: String,
    pub mods: Value,
    pub rate: f64,
    pub judgments: Value,
    pub accuracy: f64,
    pub wife: rating::Wife,
    pub score: f64,
    pub max_combo: u64,
    pub outcome: String,
    pub timestamp: String,
    pub source: String,
    pub difficulty: Value,
    pub calculation_error: Option<String>,
    pub stars: Option<f64>,
    pub pp: Option<f64>,
    pub eligible: bool,
    #[serde(skip)]
    pub map: Option<Vec<u8>>,
}

impl Play {
    pub fn from_frame(v: &[Value], source: &str) -> Self {
        let b = &v[2];
        let p = &v[3];
        let mode = p
            .pointer("/mode/name")
            .or_else(|| b.pointer("/mode/name"))
            .and_then(Value::as_str)
            .unwrap_or("mania")
            .to_owned();
        let mods = p["mods"].clone();
        let disallowed = mods["number"].as_u64().unwrap_or(0)
            & (1 << 11
                | 1 << 13
                | 1 << 7
                | 1 << 1
                | 1 << 4
                | 1 << 21
                | 1 << 30
                | 0x000f8000
                | 0x1f000000)
            != 0
            || mods["array"].as_array().is_some_and(|a| {
                a.iter().any(|m| {
                    matches!(
                        m["acronym"].as_str(),
                        Some(
                            "AT" | "RX"
                                | "AP"
                                | "EZ"
                                | "HR"
                                | "MR"
                                | "RD"
                                | "WU"
                                | "WD"
                                | "AS"
                                | "4K"
                                | "5K"
                                | "6K"
                                | "7K"
                                | "8K"
                                | "9K"
                                | "1K"
                                | "2K"
                                | "3K"
                                | "CO"
                        )
                    )
                })
            });
        Self {
            id: 0,
            checksum: b["checksum"].as_str().unwrap_or("").into(),
            title: format!(
                "{} [{}]",
                b["title"].as_str().unwrap_or("Unknown map"),
                b["version"].as_str().unwrap_or("")
            ),
            mode,
            keys: b
                .pointer("/stats/cs/converted")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as u32,
            client: v[1].as_str().unwrap_or("unknown").into(),
            rate: rating::rate(&mods),
            mods,
            judgments: p["hits"].clone(),
            accuracy: p["accuracy"].as_f64().unwrap_or(0.0),
            wife: rating::wife(
                &p["hits"],
                p.get("hitOffsets"),
                b.pointer("/stats/hitWindow"),
            ),
            score: p["score"].as_f64().unwrap_or(0.0),
            max_combo: p.pointer("/combo/max").and_then(Value::as_u64).unwrap_or(0),
            outcome: if p["failed"].as_bool().unwrap_or(false) {
                "failed"
            } else {
                "quit"
            }
            .into(),
            timestamp: Utc::now().to_rfc3339(),
            source: source.into(),
            difficulty: Value::Null,
            calculation_error: None,
            stars: b
                .pointer("/stats/stars/total")
                .or_else(|| b.pointer("/stats/stars/live"))
                .and_then(Value::as_f64),
            pp: p.pointer("/pp/current").and_then(Value::as_f64),
            eligible: !disallowed,
            map: None,
        }
    }
}

pub struct Detector {
    pub values: Vec<Value>,
    pub active: Option<Play>,
    last_time: f64,
    last_count: u64,
}
impl Default for Detector {
    fn default() -> Self {
        Self {
            values: vec![Value::Null; FIELDS.len()],
            active: None,
            last_time: 0.0,
            last_count: 0,
        }
    }
}
impl Detector {
    pub fn apply(&mut self, frame: &Value) -> Result<(Vec<Play>, bool), String> {
        let kind = frame[0].as_u64().ok_or("invalid subscription frame")?;
        if kind > 1 {
            return Err("subscription error".into());
        }
        if kind == 0 {
            self.values.fill(Value::Null);
        }
        for pair in frame[2].as_array().ok_or("invalid pairs")? {
            let id = pair[0].as_u64().ok_or("invalid selector id")? as usize;
            if id >= FIELDS.len() {
                return Err("unknown selector id".into());
            }
            self.values[id] = pair[1].clone();
        }
        if let Some(removed) = frame[3].as_array() {
            for id in removed {
                if let Some(i) = id.as_u64().filter(|i| (*i as usize) < FIELDS.len()) {
                    self.values[i as usize] = Value::Null;
                }
            }
        }
        Ok(self.update())
    }
    fn update(&mut self) -> (Vec<Play>, bool) {
        let playing = self.values[0]["number"] == 2 || self.values[0]["name"] == "play";
        let results = self.values[0]["number"] == 7 || self.values[0]["name"] == "resultScreen";
        let time = self.values[2]
            .pointer("/time/live")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let count = self.values[6].as_u64().unwrap_or(0);
        let score = self.values[3]["score"].as_f64().unwrap_or(0.0);
        let changed = self
            .active
            .as_ref()
            .is_some_and(|a| a.checksum != self.values[2]["checksum"].as_str().unwrap_or(""));
        let retry = playing
            && self.active.as_ref().is_some_and(|a| {
                (time + 500.0 < self.last_time && score < a.score) || count > self.last_count
            });
        let mut finished = Vec::new();
        if self.active.is_some() && (!playing || changed || retry) {
            let mut p = self.active.take().unwrap();
            if results && !changed {
                let r = &self.values[4];
                if rating::total(&r["hits"]) >= rating::total(&p.judgments) {
                    let judgments_changed = p.judgments != r["hits"];
                    p.judgments = r["hits"].clone();
                    p.score = r["score"].as_f64().unwrap_or(p.score);
                    p.accuracy = r["accuracy"].as_f64().unwrap_or(p.accuracy);
                    p.max_combo = r["maxCombo"].as_u64().unwrap_or(p.max_combo);
                    p.pp = r.pointer("/pp/current").and_then(Value::as_f64).or(p.pp);
                    if judgments_changed || !p.wife.model.ends_with("offsets") {
                        p.wife = rating::wife(
                            &p.judgments,
                            None,
                            self.values[2].pointer("/stats/hitWindow"),
                        );
                    }
                }
                if p.outcome != "failed" {
                    p.outcome = "passed".into();
                }
            }
            if rating::total(&p.judgments) > 0 {
                finished.push(p);
            }
        }
        let started = playing && self.active.is_none();
        if playing {
            let mut p = Play::from_frame(&self.values, "live");
            if let Some(old) = self.active.take() {
                p.map = old.map;
                p.timestamp = old.timestamp;
                p.calculation_error = old.calculation_error;
                if old.outcome == "failed" {
                    p.outcome = old.outcome;
                }
            }
            self.active = Some(p);
        }
        self.last_time = time;
        self.last_count = count;
        (finished, started)
    }
    pub fn disconnect(&mut self) -> Option<Play> {
        self.active
            .take()
            .filter(|p| rating::total(&p.judgments) > 0)
            .map(|mut p| {
                p.outcome = "interrupted".into();
                p
            })
    }
}

#[cfg(test)]
pub fn mock_frame(state: u64, score: u64, time: f64, failed: bool, md5: &str) -> Value {
    json!([0,1,[[0,{"number":state}],[1,"stable"],[2,{"checksum":md5,"title":"Test","mode":{"name":"mania"},"time":{"live":time},"stats":{"cs":{"converted":4}}}],[3,{"mode":{"name":"mania"},"score":score,"accuracy":100,"failed":failed,"hits":{"geki":score},"mods":{"rate":1.0}}],[4,{}],[5,false],[6,0]]])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequences() {
        for client in ["stable", "lazer"] {
            for (end, fail, outcome) in [
                (7, false, "passed"),
                (5, true, "failed"),
                (5, false, "quit"),
            ] {
                let mut d = Detector::default();
                let mut f = mock_frame(2, 10, 1000.0, fail, "a");
                f[2][1][1] = json!(client);
                assert!(d.apply(&f).unwrap().1);
                let pause = json!([1, 2, [[5, true]], []]);
                assert!(d.apply(&pause).unwrap().0.is_empty());
                let mut exit = mock_frame(end, 0, 0.0, false, "a");
                exit[2][1][1] = json!(client);
                let (p, _) = d.apply(&exit).unwrap();
                assert_eq!(p.len(), 1);
                assert_eq!(p[0].outcome, outcome);
                assert_eq!(p[0].score, 10.0);
                assert!(d.apply(&exit).unwrap().0.is_empty());
            }
        }
    }
    #[test]
    fn retry_and_map_change() {
        let mut d = Detector::default();
        d.apply(&mock_frame(2, 10, 1000.0, false, "a")).unwrap();
        let (p, start) = d.apply(&mock_frame(2, 0, 0.0, false, "a")).unwrap();
        assert!(start);
        assert_eq!(p[0].outcome, "quit");
        d.apply(&mock_frame(2, 5, 1000.0, false, "a")).unwrap();
        let (p, start) = d.apply(&mock_frame(2, 0, 0.0, false, "b")).unwrap();
        assert!(start);
        assert_eq!(p[0].checksum, "a");
    }
}
