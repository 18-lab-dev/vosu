use crate::{
    detect::Play,
    rating::{self, SKILLS},
};
use chrono::Utc;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::Duration,
};

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
pub struct Store {
    db: Connection,
    best: HashMap<String, Vec<Play>>,
    dan_by_chart: HashMap<String, Value>,
    counts: BTreeMap<String, i64>,
    pub profile: Value,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            return Err("skill tracker database was created by a newer version".into());
        }
        if version == 0 {
            db.execute_batch("BEGIN IMMEDIATE; CREATE TABLE plays(id INTEGER PRIMARY KEY AUTOINCREMENT, checksum TEXT NOT NULL, mode TEXT NOT NULL, dedup TEXT UNIQUE, body TEXT NOT NULL); CREATE INDEX plays_map ON plays(checksum); CREATE TABLE trend(id INTEGER PRIMARY KEY, timestamp TEXT NOT NULL, overall REAL NOT NULL); PRAGMA user_version=1; COMMIT;")?;
        }
        let counts = {
            let mut q = db.prepare("SELECT mode,COUNT(*) FROM plays GROUP BY mode")?;
            q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<std::result::Result<BTreeMap<_, _>, _>>()?
        };
        let mut store = Self {
            db,
            best: HashMap::new(),
            dan_by_chart: HashMap::new(),
            counts,
            profile: Value::Null,
        };
        let checksums = {
            let mut q = store.db.prepare("SELECT DISTINCT checksum FROM plays")?;
            q.query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for checksum in checksums {
            store.refresh_chart(&checksum)?;
        }
        store.recompute()?;
        Ok(store)
    }
    fn chart(&self, checksum: &str) -> Result<Vec<Play>> {
        let mut q = self
            .db
            .prepare("SELECT id,body FROM plays WHERE checksum=?")?;
        let rows = q.query_map([checksum], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut plays = Vec::new();
        for row in rows {
            let (id, body) = row?;
            let mut p: Play = serde_json::from_str(&body)?;
            p.id = id;
            plays.push(p);
        }
        Ok(plays)
    }
    fn refresh_chart(&mut self, checksum: &str) -> Result<()> {
        let mut rates: BTreeMap<i64, Play> = BTreeMap::new();
        let mut dan = Value::Null;
        for p in self.chart(checksum)? {
            let d = &p.difficulty["dan"];
            if p.mode == "mania"
                && p.outcome == "passed"
                && p.eligible
                && p.accuracy >= 96.0
                && d["value"]
                    .as_f64()
                    .is_some_and(|v| v > dan["value"].as_f64().unwrap_or(-1.0))
            {
                dan = d.clone();
            }
            if p.mode != "mania"
                || p.outcome != "passed"
                || !p.eligible
                || !p.difficulty["ssr"].is_object()
            {
                continue;
            }
            let key = (p.rate * 100.0).round() as i64;
            if rates
                .get(&key)
                .is_none_or(|old| old.wife.value < p.wife.value)
            {
                rates.insert(key, p);
            }
        }
        if dan.is_null() {
            self.dan_by_chart.remove(checksum);
        } else {
            self.dan_by_chart.insert(checksum.into(), dan);
        }
        let mut best: Vec<_> = rates.into_values().collect();
        best.sort_by(|a, b| ssr(b, "overall").total_cmp(&ssr(a, "overall")));
        best.truncate(2);
        if best.is_empty() {
            self.best.remove(checksum);
        } else {
            self.best.insert(checksum.into(), best);
        }
        Ok(())
    }
    fn recompute(&mut self) -> Result<()> {
        let mut ratings = serde_json::Map::new();
        let mut sum = 0.0;
        for skill in SKILLS.iter().skip(1) {
            let values: Vec<_> = self
                .best
                .values()
                .flatten()
                .map(|p| ssr(p, skill))
                .collect();
            let value = rating::aggregate(&values);
            ratings.insert((*skill).into(), json!(value));
            sum += value;
        }
        ratings.insert("overall".into(), json!(sum / 7.0));
        let counts = &self.counts;
        let dan = self
            .dan_by_chart
            .values()
            .max_by(|a, b| {
                a["value"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["value"].as_f64().unwrap_or(0.0))
            })
            .cloned()
            .unwrap_or(Value::Null);
        let updated: Option<String> = self
            .db
            .query_row(
                "SELECT timestamp FROM trend ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();
        let trend = {
            let mut q = self
                .db
                .prepare("SELECT timestamp,overall FROM trend ORDER BY id DESC LIMIT 60")?;
            q.query_map([], |r| {
                Ok(json!({"timestamp":r.get::<_,String>(0)?,"overall":r.get::<_,f64>(1)?}))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        self.profile = json!({"ratings":ratings,"dan":dan,"dan_accuracy_threshold":96.0,"dan_experimental":true,"play_counts":counts,"rated_scores":self.best.values().map(Vec::len).sum::<usize>(),"updated_at":updated,"trend":trend,"rating_model":"etterna-0.75.1-aggregate-skill","accuracy_notice":"Wife3 J4 from complete offsets; otherwise judgment-midpoint approximation. Not official Etterna ratings."});
        Ok(())
    }
    pub fn insert(&mut self, p: &mut Play) -> Result<bool> {
        let dedup = if p.source == "stable-import" {
            Some(format!(
                "{}:{}:{}:{}:{}",
                p.checksum, p.timestamp, p.score, p.max_combo, p.mods
            ))
        } else {
            None
        };
        let changed = self.db.execute(
            "INSERT OR IGNORE INTO plays(checksum,mode,dedup,body) VALUES(?,?,?,?)",
            params![p.checksum, p.mode, dedup, serde_json::to_string(&p)?],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        p.id = self.db.last_insert_rowid();
        *self.counts.entry(p.mode.clone()).or_default() += 1;
        self.changed(&p.checksum)?;
        Ok(true)
    }
    fn changed(&mut self, checksum: &str) -> Result<()> {
        self.refresh_chart(checksum)?;
        self.recompute()?;
        let timestamp = Utc::now().to_rfc3339();
        let overall = self.profile["ratings"]["overall"].as_f64().unwrap_or(0.0);
        self.db.execute(
            "INSERT INTO trend(timestamp,overall) VALUES(?,?)",
            params![timestamp, overall],
        )?;
        self.profile["updated_at"] = json!(timestamp);
        if let Some(trend) = self.profile["trend"].as_array_mut() {
            trend.insert(0, json!({"timestamp": timestamp, "overall": overall}));
            trend.truncate(60);
        }
        Ok(())
    }
    pub fn get(&self, id: i64) -> Result<Option<Play>> {
        let mut q = self.db.prepare("SELECT body FROM plays WHERE id=?")?;
        let mut rows = q.query([id])?;
        if let Some(row) = rows.next()? {
            let mut p: Play = serde_json::from_str(&row.get::<_, String>(0)?)?;
            p.id = id;
            Ok(Some(p))
        } else {
            Ok(None)
        }
    }
    pub fn delete(&mut self, id: i64) -> Result<bool> {
        if let Some(p) = self.get(id)? {
            self.db.execute("DELETE FROM plays WHERE id=?", [id])?;
            if let Some(count) = self.counts.get_mut(&p.mode) {
                *count -= 1;
                if *count == 0 {
                    self.counts.remove(&p.mode);
                }
            }
            self.changed(&p.checksum)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn history(&self, limit: u32, before: i64, mode: Option<&str>) -> Result<Vec<Play>> {
        let mut q=self.db.prepare("SELECT id,body FROM plays WHERE id<? AND (? IS NULL OR mode=?) ORDER BY id DESC LIMIT ?")?;
        let rows = q.query_map(params![before, mode, mode, limit.min(200)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, body) = row?;
            let mut p: Play = serde_json::from_str(&body)?;
            p.id = id;
            out.push(p);
        }
        Ok(out)
    }
}
fn ssr(p: &Play, skill: &str) -> f64 {
    p.difficulty["ssr"][skill]
        .as_f64()
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Detector, mock_frame};
    #[test]
    fn migrations_and_durable_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqlite");
        let mut s = Store::open(&path).unwrap();
        let mut d = Detector::default();
        d.apply(&mock_frame(2, 10, 1000.0, false, "a")).unwrap();
        let mut p = d
            .apply(&mock_frame(7, 0, 0.0, false, "a"))
            .unwrap()
            .0
            .remove(0);
        p.difficulty = json!({"ssr":{"overall":20,"stream":20}});
        assert!(s.insert(&mut p).unwrap());
        assert_eq!(s.profile["play_counts"]["mania"], 1);
        drop(s);
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.get(p.id).unwrap().unwrap().score, 10.0);
        assert!(s.delete(p.id).unwrap());
        assert!(s.history(20, i64::MAX, None).unwrap().is_empty());
        assert_eq!(s.profile["rated_scores"], 0);
    }
    #[test]
    fn best_scores_dan_and_import_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("scores.sqlite")).unwrap();
        let mut detector = Detector::default();
        detector
            .apply(&mock_frame(2, 10, 1000.0, false, "a"))
            .unwrap();
        let mut play = detector
            .apply(&mock_frame(7, 0, 0.0, false, "a"))
            .unwrap()
            .0
            .remove(0);
        let skills: serde_json::Map<String, Value> = SKILLS
            .iter()
            .map(|key| ((*key).into(), json!(20.0)))
            .collect();
        play.difficulty =
            json!({"ssr":skills,"dan":{"label":"Test 5","value":5.0,"experimental":true}});
        play.source = "stable-import".into();
        assert!(store.insert(&mut play).unwrap());
        assert!(!store.insert(&mut play).unwrap());
        assert!(
            (store.profile["ratings"]["overall"].as_f64().unwrap() - 15.077999115).abs() < 0.0001
        );
        assert_eq!(store.profile["dan"]["value"], 5.0);
        let first_id = play.id;
        play.timestamp = "2026-10-21T11:00:00Z".into();
        play.wife.value = 0.9;
        play.difficulty["dan"]["value"] = json!(7.0);
        assert!(store.insert(&mut play).unwrap());
        assert_eq!(store.profile["rated_scores"], 1);
        assert_eq!(store.profile["play_counts"]["mania"], 2);
        assert_eq!(store.profile["dan"]["value"], 7.0);
        assert!(store.delete(play.id).unwrap());
        assert_eq!(store.profile["dan"]["value"], 5.0);
        assert!(store.delete(first_id).unwrap());
        assert!(store.profile["dan"].is_null());
    }

    #[test]
    fn rejects_future_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 2)
            .unwrap();
        assert!(Store::open(&path).is_err());
    }
}
