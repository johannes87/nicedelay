use std::path::PathBuf;
use std::{env, fs, io};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Hardware H.264 encoders nicedelay knows how to drive. There is no software encoder:
/// without one of these, nicedelay can't stream.
pub const ENCODERS: &[&str] = &["h264_videotoolbox", "h264_nvenc", "h264_amf", "h264_qsv", "h264_vaapi", "h264_mf"];

const ALLOWED_FPS: &[f64] = &[24.0, 25.0, 30.0, 48.0, 50.0, 60.0];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub http_host: String,
    pub http_port: u16,
    pub ingest: IngestConfig,
    pub output: OutputConfig,
    pub destinations: Vec<Destination>,
    pub buffer_minutes: f64,
    /// default delay used by "start delay"
    pub delay_seconds: f64,
    pub transition: TransitionConfig,
    pub badge: BadgePosition,
    pub reconnect_grace_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IngestConfig {
    pub host: String,
    pub port: u16,
    pub app: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// kbps
    pub video_bitrate: u32,
    /// kbps
    pub audio_bitrate: u32,
    /// "auto" or one of [`ENCODERS`]
    pub encoder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub name: String,
    /// rtmp(s):// server URL, or a local file path (handy for testing)
    pub url: String,
    pub key: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionConfig {
    pub sound: bool,
    pub sound_volume: f64,
}

/// Where the on-stream badge sits: 0..1 across the room it has to move, so 0,0 is top
/// left and 1,1 bottom right.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BadgePosition {
    pub x: f64,
    pub y: f64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            http_host: "127.0.0.1".into(),
            http_port: 8080,
            ingest: IngestConfig { host: "127.0.0.1".into(), port: 1935, app: "live".into(), key: "nicedelay".into() },
            output: OutputConfig {
                width: 1920,
                height: 1080,
                fps: 30,
                video_bitrate: 6000,
                audio_bitrate: 160,
                encoder: "auto".into(),
            },
            destinations: vec![],
            buffer_minutes: 10.0,
            delay_seconds: 10.0,
            transition: TransitionConfig { sound: true, sound_volume: 0.6 },
            badge: BadgePosition { x: 0.0, y: 0.0 },
            reconnect_grace_seconds: 15.0,
        }
    }
}

fn obj(v: Option<&Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    }
}

fn to_number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.trim().is_empty() => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

fn num(v: Option<&Value>, min: f64, max: f64, dflt: f64) -> f64 {
    to_number(v).map_or(dflt, |n| n.clamp(min, max))
}

fn str_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn bool_of(v: Option<&Value>, dflt: bool) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        _ => dflt,
    }
}

/// Letters, digits, `_`, `.` and `-` only (safe inside an RTMP URL path).
fn url_token(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')).collect()
}

fn even(n: f64) -> u32 {
    ((n / 2.0).round() * 2.0) as u32
}

/// Builds a complete, sane Config from untrusted JSON (file or API), falling back to defaults.
pub fn validate(input: &Value) -> Config {
    let d = Config::default();
    let c = obj(Some(input));
    let ingest = obj(c.get("ingest"));
    let output = obj(c.get("output"));
    let transition = obj(c.get("transition"));
    let badge = obj(c.get("badge"));
    let fps = num(output.get("fps"), 1.0, 240.0, f64::from(d.output.fps));
    let buffer_minutes = num(c.get("bufferMinutes"), 1.0, 60.0, d.buffer_minutes);
    let encoder = str_of(output.get("encoder"));
    let key = url_token(&str_of(ingest.get("key")));
    let app = url_token(&str_of(ingest.get("app")));
    let destinations = match c.get("destinations") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|x| {
                let x = x.as_object()?;
                let url = str_of(x.get("url"));
                (!url.is_empty()).then(|| Destination {
                    name: str_of(x.get("name")).chars().take(60).collect(),
                    url,
                    key: str_of(x.get("key")),
                    enabled: x.get("enabled") != Some(&Value::Bool(false)),
                })
            })
            .collect(),
        _ => vec![],
    };
    Config {
        http_host: Some(str_of(c.get("httpHost"))).filter(|s| !s.is_empty()).unwrap_or(d.http_host),
        http_port: num(c.get("httpPort"), 1.0, 65_535.0, f64::from(d.http_port)) as u16,
        ingest: IngestConfig {
            host: Some(str_of(ingest.get("host"))).filter(|s| !s.is_empty()).unwrap_or(d.ingest.host),
            port: num(ingest.get("port"), 1.0, 65_535.0, f64::from(d.ingest.port)) as u16,
            app: if app.is_empty() { d.ingest.app } else { app },
            key: if key.is_empty() { d.ingest.key } else { key },
        },
        output: OutputConfig {
            width: even(num(output.get("width"), 320.0, 7680.0, f64::from(d.output.width))),
            height: even(num(output.get("height"), 180.0, 4320.0, f64::from(d.output.height))),
            fps: if ALLOWED_FPS.contains(&fps) { fps as u32 } else { d.output.fps },
            video_bitrate: num(output.get("videoBitrate"), 500.0, 50_000.0, f64::from(d.output.video_bitrate)) as u32,
            audio_bitrate: num(output.get("audioBitrate"), 64.0, 320.0, f64::from(d.output.audio_bitrate)) as u32,
            encoder: if ENCODERS.contains(&encoder.as_str()) { encoder } else { d.output.encoder },
        },
        destinations,
        buffer_minutes,
        delay_seconds: num(c.get("delaySeconds"), 1.0, buffer_minutes * 60.0, d.delay_seconds),
        transition: TransitionConfig {
            sound: bool_of(transition.get("sound"), d.transition.sound),
            sound_volume: num(transition.get("soundVolume"), 0.0, 1.0, d.transition.sound_volume),
        },
        badge: BadgePosition {
            x: num(badge.get("x"), 0.0, 1.0, d.badge.x),
            y: num(badge.get("y"), 0.0, 1.0, d.badge.y),
        },
        reconnect_grace_seconds: num(c.get("reconnectGraceSeconds"), 0.0, 300.0, d.reconnect_grace_seconds),
    }
}

/// `NICEDELAY_CONFIG`, or `config.json` in the platform's config directory.
pub fn config_path() -> PathBuf {
    if let Some(p) = env::var_os("NICEDELAY_CONFIG") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("", "", "nicedelay")
        .map(|d| d.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

pub fn load() -> Config {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => validate(&v),
            Err(err) => {
                tracing::warn!("could not read {}: {err}", path.display());
                Config::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!("could not read {}: {err}", path.display());
            }
            Config::default()
        }
    }
}

pub fn save(cfg: &Config) -> io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(cfg).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

/// `cfg` with the fields present in `patch` (a partial Config, as JSON) applied, validated.
pub fn merge(cfg: &Config, patch: &Value) -> Config {
    let mut base = serde_json::to_value(cfg).unwrap_or(Value::Null);
    if let (Value::Object(b), Value::Object(p)) = (&mut base, patch) {
        for (k, v) in p {
            b.insert(k.clone(), v.clone());
        }
    }
    validate(&base)
}

/// The full stream URL for a destination: server URL plus stream key.
pub fn destination_url(d: &Destination) -> String {
    let lower = d.url.to_ascii_lowercase();
    if !(lower.starts_with("rtmp://") || lower.starts_with("rtmps://")) || d.key.is_empty() {
        return d.url.clone();
    }
    format!("{}/{}", d.url.trim_end_matches('/'), d.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fills_defaults_and_rejects_junk() {
        let c = validate(&json!({
            "output": { "width": 1919, "fps": 29, "encoder": "nope", "videoBitrate": "4500" },
            "destinations": [{ "url": "  rtmps://x/app " }, { "url": "" }, "garbage"],
            "ingest": { "key": "a b/c" },
            "delaySeconds": 99_999,
            "bufferMinutes": 2,
            "badge": { "x": 1.5, "y": "top" },
        }));
        assert_eq!(c.output.width, 1920);
        assert_eq!(c.output.fps, 30);
        assert_eq!(c.output.encoder, "auto");
        assert_eq!(c.output.video_bitrate, 4500);
        assert_eq!(
            c.destinations,
            vec![Destination { name: String::new(), url: "rtmps://x/app".into(), key: String::new(), enabled: true }]
        );
        assert_eq!(c.ingest.key, "abc");
        assert_eq!(c.delay_seconds, 120.0);
        assert_eq!(c.badge, BadgePosition { x: 1.0, y: 0.0 });
    }

    #[test]
    fn keeps_a_valid_config_unchanged() {
        let c = Config::default();
        assert_eq!(validate(&serde_json::to_value(&c).unwrap()), c);
    }

    #[test]
    fn merges_a_partial_update() {
        let c = merge(&Config::default(), &json!({ "delaySeconds": 30, "badge": { "x": 0.5, "y": 1 } }));
        assert_eq!(c.delay_seconds, 30.0);
        assert_eq!(c.badge, BadgePosition { x: 0.5, y: 1.0 });
        assert_eq!(c.output, Config::default().output);
    }

    #[test]
    fn appends_the_stream_key() {
        let d =
            |url: &str, key: &str| Destination { name: String::new(), url: url.into(), key: key.into(), enabled: true };
        assert_eq!(destination_url(&d("rtmps://a.example/app/", "k")), "rtmps://a.example/app/k");
        assert_eq!(destination_url(&d("rtmp://a/app", "")), "rtmp://a/app");
        assert_eq!(destination_url(&d("/tmp/out.flv", "k")), "/tmp/out.flv");
    }
}
