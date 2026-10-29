//! The account's games and trophies, from the PSN API (the same mobile-app login the console list uses).
//!
//! Sony does not tell third parties which games are *installed* on a console. What the account exposes is the games it has played
//! (`gamelist`) and the trophy progress per game (`trophy`); this module reads both and joins them by name. Everything that ends up on
//! screen is cleaned and length-limited, and cover images may only come from Sony's own image hosts.

use crate::api::{check, Api, ApiError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::oauth::PsnError;
use crate::store::SecretStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BASE: &str = "https://m.np.playstation.com/api";
const GAMES_PAGE: usize = 200;
const TROPHY_PAGE: usize = 100;
const MAX_PAGES: usize = 8;
pub const MAX_ART_BYTES: usize = 900 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrophyCounts {
    pub platinum: u32,
    pub gold: u32,
    pub silver: u32,
    pub bronze: u32,
}

impl TrophyCounts {
    pub fn total(&self) -> u32 {
        self.platinum + self.gold + self.silver + self.bronze
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameTrophies {
    /// 0..=100
    pub progress: u8,
    pub earned: TrophyCounts,
    pub total: TrophyCounts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    /// The PlayStation title id as the console lists it (`PPSA01234_00`), when known.
    #[serde(default)]
    pub title_id: Option<String>,
    pub name: String,
    pub platform: String,
    pub image_url: Option<String>,
    pub last_played: Option<String>,
    pub play_minutes: Option<u32>,
    pub trophies: Option<GameTrophies>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrophySummary {
    pub level: u32,
    /// Progress to the next level, 0..=100.
    pub progress: u8,
    pub earned: TrophyCounts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    pub games: Vec<Game>,
    pub summary: Option<TrophySummary>,
    /// What each of the account's consoles has installed (empty when Sony does not report it).
    #[serde(default)]
    pub consoles: Vec<ConsoleTitles>,
}

/// One PS5 of the account and the title ids installed on it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleTitles {
    pub duid: String,
    pub name: String,
    pub installed: Vec<InstalledTitle>,
    pub updated: Option<String>,
}

/// A title installed on a console, as the console reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledTitle {
    /// Normalised (no `_00`): `PPSA01234`.
    pub title_id: String,
    pub name: String,
    /// `PS5` or `PS4`.
    pub platform: String,
    /// `game` or `media` (Netflix and the like).
    pub kind: String,
    pub size_bytes: u64,
    pub last_access: Option<String>,
}

fn get(url: String, token: &str) -> HttpRequest {
    HttpRequest {
        method: "GET",
        url,
        headers: vec![
            ("Accept-Language".into(), "en-US".into()),
            ("User-Agent".into(), "RpNetHttpUtilImpl".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: vec![],
    }
}

pub fn summary_request(token: &str) -> HttpRequest {
    get(format!("{BASE}/trophy/v1/users/me/trophySummary"), token)
}
pub fn trophy_titles_request(token: &str, offset: usize) -> HttpRequest {
    get(format!("{BASE}/trophy/v1/users/me/trophyTitles?limit={TROPHY_PAGE}&offset={offset}"), token)
}
pub fn games_request(token: &str, offset: usize) -> HttpRequest {
    get(
        format!(
            "{BASE}/gamelist/v2/users/me/titles?categories=ps4_game,ps5_native_game&limit={GAMES_PAGE}&offset={offset}"
        ),
        token,
    )
}

fn clean(s: &str, max: usize) -> String {
    crate::oauth::clean_text(s, max)
}

fn counts(v: Option<&Value>) -> TrophyCounts {
    let n = |k: &str| v.and_then(|v| v.get(k)).and_then(Value::as_u64).unwrap_or(0).min(100_000) as u32;
    TrophyCounts { platinum: n("platinum"), gold: n("gold"), silver: n("silver"), bronze: n("bronze") }
}

fn json(resp: &HttpResponse, what: &'static str) -> Result<Value, PsnError> {
    check(resp)?;
    serde_json::from_slice(&resp.body).map_err(|_| PsnError::BadResponse(what))
}

pub fn parse_summary(resp: &HttpResponse) -> Result<TrophySummary, PsnError> {
    let j = json(resp, "trophy summary is not JSON")?;
    Ok(TrophySummary {
        level: j.get("trophyLevel").and_then(Value::as_u64).unwrap_or(0).min(10_000) as u32,
        progress: j.get("progress").and_then(Value::as_u64).unwrap_or(0).min(100) as u8,
        earned: counts(j.get("earnedTrophies")),
    })
}

/// One page of trophy titles; the second value is the next offset (None on the last page).
pub fn parse_trophy_titles(resp: &HttpResponse) -> Result<(Vec<Game>, Option<usize>), PsnError> {
    let j = json(resp, "trophy list is not JSON")?;
    let games = j
        .get("trophyTitles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let name = clean(t.get("trophyTitleName")?.as_str()?, 120);
                    if name.is_empty() {
                        return None;
                    }
                    let earned = counts(t.get("earnedTrophies"));
                    let total = counts(t.get("definedTrophies"));
                    Some(Game {
                        title_id: None,
                        name,
                        platform: clean(t.get("trophyTitlePlatform").and_then(Value::as_str).unwrap_or(""), 24),
                        image_url: t.get("trophyTitleIconUrl").and_then(Value::as_str).map(str::to_string),
                        last_played: t.get("lastUpdatedDateTime").and_then(Value::as_str).map(|s| clean(s, 40)),
                        play_minutes: None,
                        trophies: Some(GameTrophies {
                            progress: t.get("progress").and_then(Value::as_u64).unwrap_or(0).min(100) as u8,
                            earned,
                            total,
                        }),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((games, j.get("nextOffset").and_then(Value::as_u64).map(|n| n as usize)))
}

/// "PT3H12M5S" -> minutes.
pub fn parse_duration_minutes(s: &str) -> Option<u32> {
    let rest = s.strip_prefix("PT")?;
    let (mut total, mut num) = (0u64, String::new());
    for c in rest.chars() {
        match c {
            '0'..='9' => num.push(c),
            'H' => {
                total = total.saturating_add(num.parse::<u64>().ok()?.saturating_mul(60));
                num.clear();
            }
            'M' => {
                total = total.saturating_add(num.parse::<u64>().ok()?);
                num.clear();
            }
            'S' => num.clear(),
            _ => return None,
        }
    }
    Some(total.min(10_000_000) as u32)
}

pub fn parse_games(resp: &HttpResponse) -> Result<(Vec<Game>, Option<usize>), PsnError> {
    let j = json(resp, "game list is not JSON")?;
    let games = j
        .get("titles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let name = clean(t.get("name")?.as_str()?, 120);
                    if name.is_empty() {
                        return None;
                    }
                    // the poster: Sony's portrait banner if the game has one, else the gamehub cover, else the key art, else any image
                    let media = t.get("media").and_then(|m| m.get("images")).and_then(Value::as_array);
                    let cover = media
                        .and_then(|imgs| {
                            ["PORTRAIT_BANNER", "GAMEHUB_COVER_ART", "MASTER"]
                                .iter()
                                .find_map(|kind| {
                                    imgs.iter().find(|i| i.get("type").and_then(Value::as_str) == Some(kind))
                                })
                                .or_else(|| imgs.first())
                        })
                        .and_then(|i| i.get("url"))
                        .and_then(Value::as_str);
                    let image = cover
                        .or_else(|| t.get("imageUrl").and_then(Value::as_str))
                        .or_else(|| t.get("localizedImageUrl").and_then(Value::as_str))
                        .or_else(|| t.get("image").and_then(Value::as_str))
                        .map(str::to_string);
                    let platform = match t.get("category").and_then(Value::as_str).unwrap_or("") {
                        c if c.starts_with("ps5") => "PS5",
                        c if c.starts_with("ps4") => "PS4",
                        _ => "",
                    };
                    Some(Game {
                        title_id: t
                            .get("titleId")
                            .and_then(Value::as_str)
                            .map(|s| norm_title(&clean(s, 24)))
                            .filter(|s| !s.is_empty()),
                        name,
                        platform: platform.into(),
                        image_url: image,
                        last_played: t.get("lastPlayedDateTime").and_then(Value::as_str).map(|s| clean(s, 40)),
                        play_minutes: t.get("playDuration").and_then(Value::as_str).and_then(parse_duration_minutes),
                        trophies: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((games, j.get("nextOffset").and_then(Value::as_u64).map(|n| n as usize)))
}

/// Lower-case letters and digits only, so "Ghost of Yōtei™" and "GHOST OF YOTEI" meet.
fn key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Games that were played first (most recent first), each with its trophy progress when there is one. Trophy titles with no matching
/// played game are kept too (a game can be played elsewhere), after the played ones.
pub fn merge(played: Vec<Game>, trophies: Vec<Game>) -> Vec<Game> {
    let mut left: Vec<Game> = trophies;
    // one tile per title: sort first (newest first) so the kept duplicate is the most recently played one
    let mut played = played;
    played.sort_by(|a, b| b.last_played.cmp(&a.last_played)); // ISO-8601 sorts as text
    let mut seen = std::collections::HashSet::new();
    played.retain(|g| seen.insert(format!("{}|{}", key(&g.name), g.platform)));
    let mut out: Vec<Game> = vec![];
    for mut g in played {
        let k = key(&g.name);
        // same name and platform first; otherwise a name-only match when it is unambiguous
        let pos = left.iter().position(|t| key(&t.name) == k && t.platform == g.platform).or_else(|| {
            let mut it = left.iter().enumerate().filter(|(_, t)| key(&t.name) == k);
            match (it.next(), it.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        });
        if let Some(i) = pos {
            let t = left.remove(i);
            g.trophies = t.trophies;
            if g.image_url.is_none() {
                g.image_url = t.image_url;
            }
        }
        out.push(g);
    }
    left.sort_by(|a, b| b.last_played.cmp(&a.last_played));
    out.extend(left);
    let mut seen = std::collections::HashSet::new();
    out.retain(|g| seen.insert(format!("{}|{}", key(&g.name), g.platform)));
    out
}

/// Only Sony's own image hosts, over HTTPS: the app fetches whatever URL the API names, so it must not be a way to reach anything else.
pub fn art_url_allowed(url: &str) -> bool {
    const SUFFIXES: [&str; 3] = [".playstation.com", ".playstation.net", ".sonyentertainmentnetwork.com"];
    if url.len() > 2048 || !url.is_ascii() || url.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'\\') {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://") else { return false };
    // our own strict reading of the authority: conservative host characters only, no userinfo, port or escapes
    let raw_host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    let host_ok = !raw_host.is_empty()
        && raw_host.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && raw_host.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-'));
    if !host_ok || !SUFFIXES.iter().any(|s| raw_host.ends_with(s)) {
        return false;
    }
    // and the real URL parser must agree with that reading
    let Ok(u) = url::Url::parse(url) else { return false };
    u.scheme() == "https"
        && u.username().is_empty()
        && u.password().is_none()
        && u.port().is_none()
        && matches!(u.host(), Some(url::Host::Domain(d)) if d == raw_host)
}

/// Covers are shown about 200 px wide: ask Sony's image service to scale to a width, keeping the picture's proportions (its `thumb=true`
/// option crops to a square, which cut the posters), instead of the multi-megapixel original.
pub fn thumbnail_url(url: &str) -> String {
    if url.contains('?') || !url.starts_with("https://image.api.playstation.com/") {
        return url.to_string();
    }
    format!("{url}?w=400")
}

impl<H: HttpClient + ?Sized, S: SecretStore + ?Sized> Api<'_, H, S> {
    /// GET with the stored token; a token that is turned down is refreshed once. A second refusal drops the sign-in.
    fn authed(&self, now: u64, req: impl Fn(&str) -> HttpRequest) -> Result<HttpResponse, ApiError> {
        let token = self.access_token(now, false)?;
        let resp = self.call(&req(&token))?;
        if !matches!(resp.status, 401 | 403) {
            return Ok(resp);
        }
        let token = self.access_token(now, true)?;
        let resp = self.call(&req(&token))?;
        if matches!(resp.status, 401 | 403) {
            // This login may simply not be allowed to read this endpoint: report that, but keep the sign-in for the console list.
            return Err(ApiError::Psn(PsnError::Rejected { status: resp.status }));
        }
        Ok(resp)
    }

    /// The account's games and trophy summary. Each part that fails is skipped; only if nothing at all could be read is it an error.
    pub fn library(&self, now: u64) -> Result<Library, ApiError> {
        let summary = self.authed(now, summary_request).and_then(|r| Ok(parse_summary(&r)?));
        let mut trophies = vec![];
        let mut trophy_err = None;
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            match self.authed(now, |t| trophy_titles_request(t, offset)).and_then(|r| Ok(parse_trophy_titles(&r)?)) {
                Ok((page, next)) => {
                    trophies.extend(page);
                    match next {
                        Some(n) if n > offset => offset = n,
                        _ => break,
                    }
                }
                Err(e) => {
                    trophy_err = Some(e);
                    break;
                }
            }
        }
        let mut played = vec![];
        let mut games_err = None;
        let mut offset = 0;
        for _ in 0..MAX_PAGES {
            match self.authed(now, |t| games_request(t, offset)).and_then(|r| Ok(parse_games(&r)?)) {
                Ok((page, next)) => {
                    played.extend(page);
                    match next {
                        Some(n) if n > offset => offset = n,
                        _ => break,
                    }
                }
                Err(e) => {
                    games_err = Some(e);
                    break;
                }
            }
        }
        if summary.is_err() && trophies.is_empty() && played.is_empty() {
            return Err(games_err.or(trophy_err).or(summary.err()).unwrap_or(ApiError::NotSignedIn));
        }
        // what is installed on each console (best effort: a login that may not read it just gets the played-games list)
        let consoles = self.installed_titles(now).unwrap_or_default();
        Ok(Library { games: merge(played, trophies), summary: summary.ok(), consoles })
    }

    /// One cover image, as raw bytes. Only Sony's image hosts are contacted and only small image files are accepted.
    pub fn fetch_art(&self, url: &str) -> Result<(Vec<u8>, String), String> {
        if !art_url_allowed(url) {
            return Err("that image host is not allowed".into());
        }
        let req = HttpRequest {
            method: "GET",
            url: thumbnail_url(url),
            headers: vec![("Accept".into(), "image/*".into())],
            body: vec![],
        };
        let resp = self.call(&req).map_err(|e| e.to_string())?;
        if resp.status != 200 {
            return Err(format!("image download failed ({})", resp.status));
        }
        let ctype = resp.header("content-type").unwrap_or("").to_ascii_lowercase();
        let kind = if ctype.starts_with("image/png") {
            "image/png"
        } else if ctype.starts_with("image/jpeg") || ctype.starts_with("image/jpg") {
            "image/jpeg"
        } else if ctype.starts_with("image/webp") {
            "image/webp"
        } else {
            return Err("not a supported image".into());
        };
        if resp.body.is_empty() || resp.body.len() > MAX_ART_BYTES {
            return Err("image has an unusual size".into());
        }
        Ok((resp.body, kind.into()))
    }
}

// ---- installed titles and launching a title (the console's own cloud-assisted-navigation commands) ----------------------------------
const CLIENTS: &str = "https://web.np.playstation.com/api/cloudAssistedNavigation/v2/users/me/clients";
const COMMANDS: &str = "https://web.np.playstation.com/api/cloudAssistedNavigation/v2/users/me/commands";

/// `PPSA01234_00` and `PPSA01234` name the same title.
pub fn norm_title(id: &str) -> String {
    let id = id.trim();
    id.strip_suffix("_00").unwrap_or(id).to_ascii_uppercase()
}

pub fn installed_request(token: &str) -> HttpRequest {
    get(format!("{CLIENTS}?platform=PS5&includeFields=device,systemData&limit=10&offset=0"), token)
}

/// The account's PS5s with what is installed on each. Clients without a `duid` are dropped; ids are normalised.
pub fn parse_installed(resp: &HttpResponse) -> Result<Vec<ConsoleTitles>, PsnError> {
    let j = json(resp, "console list is not JSON")?;
    Ok(j.get("clients")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let duid = c.get("duid")?.as_str()?.trim();
                    if duid.is_empty() || duid.len() > 128 || !duid.is_ascii() {
                        return None;
                    }
                    let name = c
                        .get("device")
                        .and_then(|d| d.get("name"))
                        .and_then(Value::as_str)
                        .map(|n| clean(n, 64))
                        .unwrap_or_default();
                    let titles = c.get("systemData").and_then(|d| d.get("installedTitles"));
                    let mut installed: Vec<InstalledTitle> = titles
                        .and_then(|t| t.get("titles"))
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| {
                                    let id = norm_title(
                                        t.get("npTitleId").or_else(|| t.get("titleId")).and_then(Value::as_str)?,
                                    );
                                    if id.is_empty()
                                        || id.len() > 24
                                        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                                    {
                                        return None;
                                    }
                                    let text = |k: &str| {
                                        t.get(k).and_then(Value::as_str).map(|s| clean(s, 120)).unwrap_or_default()
                                    };
                                    Some(InstalledTitle {
                                        name: Some(text("comment"))
                                            .filter(|n| !n.is_empty())
                                            .unwrap_or_else(|| id.clone()),
                                        title_id: id,
                                        platform: text("psPlatform"),
                                        kind: if text("displayLocationSpace") == "media" {
                                            "media".into()
                                        } else {
                                            "game".into()
                                        },
                                        size_bytes: t.get("size").and_then(Value::as_u64).unwrap_or(0),
                                        last_access: Some(text("lastAccessDateTime")).filter(|s| !s.is_empty()),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    installed.sort_by(|a, b| b.last_access.cmp(&a.last_access).then_with(|| a.name.cmp(&b.name)));
                    installed.dedup_by(|a, b| a.title_id == b.title_id);
                    Some(ConsoleTitles {
                        duid: duid.to_string(),
                        name,
                        installed,
                        updated: titles
                            .and_then(|t| t.get("updatedDateTime"))
                            .and_then(Value::as_str)
                            .map(|s| clean(s, 40)),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// What a launch command is doing on the console.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CommandStatus {
    pub status: String,
    pub result: String,
    pub action: String,
    pub action_state: String,
}

pub fn launch_request(token: &str, platform: &str, duid: &str, title_id: &str) -> HttpRequest {
    let body = serde_json::json!({ "commandDetail": {
        "platform": platform, "duid": duid, "commandType": "launchTitle",
        "parameters": { "titleIds": [norm_title(title_id)] } } });
    HttpRequest {
        method: "POST",
        url: COMMANDS.into(),
        headers: vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/json; charset=utf-8".into()),
            ("User-Agent".into(), "RpNetHttpUtilImpl".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: body.to_string().into_bytes(),
    }
}

pub fn parse_command_id(resp: &HttpResponse) -> Result<String, PsnError> {
    let j = json(resp, "launch answer is not JSON")?;
    j.get("commandId")
        .and_then(Value::as_str)
        .map(|s| clean(s, 128))
        .filter(|s| !s.is_empty())
        .ok_or(PsnError::BadResponse("the console did not accept the launch command"))
}

pub fn status_request(token: &str, platform: &str, command_id: &str) -> HttpRequest {
    get(
        format!(
            "{COMMANDS}?platform={}&commandIds={}&includeFields=actions,commandDetail&commandStatus=executing,completed",
            crate::oauth::url_encode(platform),
            crate::oauth::url_encode(command_id)
        ),
        token,
    )
}

pub fn parse_status(resp: &HttpResponse) -> Result<CommandStatus, PsnError> {
    let j = json(resp, "command status is not JSON")?;
    let c = j
        .get("commands")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .or_else(|| j.get("command"))
        .or(Some(&j).filter(|j| j.get("commandStatus").is_some()))
        .ok_or(PsnError::BadResponse("command status carried no command"))?;
    let action = c.get("actions").and_then(Value::as_array).and_then(|a| a.first()).and_then(|a| a.get("action"));
    let txt = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k)).and_then(Value::as_str).map(|s| clean(s, 64)).unwrap_or_default()
    };
    Ok(CommandStatus {
        status: txt(Some(c), "commandStatus"),
        result: txt(Some(c), "commandResult"),
        action: txt(action, "actionType"),
        action_state: txt(action, "actionState"),
    })
}

impl<H: HttpClient + ?Sized, S: SecretStore + ?Sized> Api<'_, H, S> {
    /// Installed titles of every PS5 on the account.
    pub fn installed_titles(&self, now: u64) -> Result<Vec<ConsoleTitles>, ApiError> {
        Ok(parse_installed(&self.authed(now, installed_request)?)?)
    }

    /// Ask a console to start a title. Returns the command id to follow with [`Api::command_status`].
    pub fn launch_title(&self, now: u64, platform: &str, duid: &str, title_id: &str) -> Result<String, ApiError> {
        if duid.is_empty() || title_id.trim().is_empty() || platform.is_empty() {
            return Err(ApiError::Psn(PsnError::BadResponse("missing console or title")));
        }
        Ok(parse_command_id(&self.authed(now, |t| launch_request(t, platform, duid, title_id))?)?)
    }

    pub fn command_status(&self, now: u64, platform: &str, command_id: &str) -> Result<CommandStatus, ApiError> {
        Ok(parse_status(&self.authed(now, |t| status_request(t, platform, command_id))?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(body: &str) -> HttpResponse {
        HttpResponse { status: 200, headers: vec![], body: body.as_bytes().to_vec() }
    }

    #[test]
    fn summary_parses_level_progress_and_counts() {
        let s = parse_summary(&ok(r#"{"accountId":"1","trophyLevel":312,"progress":47,"tier":4,"earnedTrophies":{"bronze":900,"silver":300,"gold":120,"platinum":18}}"#)).unwrap();
        assert_eq!((s.level, s.progress), (312, 47));
        assert_eq!(s.earned, TrophyCounts { platinum: 18, gold: 120, silver: 300, bronze: 900 });
    }

    #[test]
    fn missing_or_odd_fields_do_not_fail_or_overflow() {
        let s = parse_summary(&ok(r#"{"trophyLevel":99999999,"progress":900}"#)).unwrap();
        assert_eq!((s.level, s.progress), (10_000, 100));
        assert_eq!(s.earned.total(), 0);
        assert!(parse_summary(&ok("not json")).is_err());
        assert!(parse_summary(&HttpResponse { status: 403, headers: vec![], body: vec![] }).is_err());
    }

    #[test]
    fn trophy_titles_carry_progress_and_icon() {
        let (g, next) = parse_trophy_titles(&ok(r#"{"trophyTitles":[{"npCommunicationId":"NPWR1","trophyTitleName":"Ghost of Yōtei","trophyTitleIconUrl":"https://image.api.playstation.com/a.png","trophyTitlePlatform":"PS5","progress":42,"earnedTrophies":{"bronze":10,"silver":5,"gold":2,"platinum":0},"definedTrophies":{"bronze":30,"silver":15,"gold":8,"platinum":1},"lastUpdatedDateTime":"2026-10-20T10:00:00Z"},{"trophyTitleName":"   "}],"nextOffset":100,"totalItemCount":250}"#)).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(next, Some(100));
        let t = g[0].trophies.as_ref().unwrap();
        assert_eq!((t.progress, t.earned.bronze, t.total.platinum), (42, 10, 1));
        assert_eq!(g[0].image_url.as_deref(), Some("https://image.api.playstation.com/a.png"));
    }

    #[test]
    fn played_games_prefer_the_portrait_poster_then_the_cover_and_read_the_play_time() {
        let (g, _) = parse_games(&ok(r#"{"titles":[{"titleId":"PPSA1_00","name":"Astro Bot","category":"ps5_native_game","imageUrl":"https://image.api.playstation.com/plain.png","media":{"images":[{"url":"https://image.api.playstation.com/x.png","type":"SCREENSHOT"},{"url":"https://image.api.playstation.com/cover.png","type":"GAMEHUB_COVER_ART"},{"url":"https://image.api.playstation.com/poster.png","type":"PORTRAIT_BANNER"}]},"lastPlayedDateTime":"2026-10-22T09:00:00Z","playDuration":"PT12H30M15S"}]}"#)).unwrap();
        assert_eq!(g[0].image_url.as_deref(), Some("https://image.api.playstation.com/poster.png"));
        // without a portrait banner the gamehub cover is used
        let (g2, _) = parse_games(&ok(r#"{"titles":[{"name":"B","media":{"images":[{"url":"https://image.api.playstation.com/s.png","type":"SCREENSHOT"},{"url":"https://image.api.playstation.com/c.png","type":"GAMEHUB_COVER_ART"}]}}]}"#)).unwrap();
        assert_eq!(g2[0].image_url.as_deref(), Some("https://image.api.playstation.com/c.png"));
        assert_eq!((g[0].platform.as_str(), g[0].play_minutes), ("PS5", Some(750)));
    }

    #[test]
    fn durations_parse_and_garbage_is_ignored() {
        assert_eq!(parse_duration_minutes("PT45M"), Some(45));
        assert_eq!(parse_duration_minutes("PT2H"), Some(120));
        assert_eq!(parse_duration_minutes("P1D"), None);
        assert_eq!(parse_duration_minutes("PTxH"), None);
    }

    fn game(name: &str, last: &str, tro: Option<u8>) -> Game {
        Game {
            title_id: None,
            name: name.into(),
            platform: "PS5".into(),
            image_url: None,
            last_played: Some(last.into()),
            play_minutes: None,
            trophies: tro.map(|p| GameTrophies {
                progress: p,
                earned: TrophyCounts::default(),
                total: TrophyCounts::default(),
            }),
        }
    }

    #[test]
    fn merge_joins_by_name_orders_by_recency_and_keeps_trophy_only_titles() {
        let played = vec![game("Old Game", "2026-01-01", None), game("Ghost of Yōtei", "2026-10-22", None)];
        let trophies = vec![game("GHOST OF YŌTEI", "2026-10-21", Some(42)), game("Elsewhere", "2026-05-05", Some(7))];
        let m = merge(played, trophies);
        let names: Vec<_> = m.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Ghost of Yōtei", "Old Game", "Elsewhere"]);
        assert_eq!(m[0].trophies.as_ref().unwrap().progress, 42);
        assert!(m[1].trophies.is_none());
    }

    #[test]
    fn installed_titles_are_read_per_console_and_normalised() {
        let r = parse_installed(&ok(r#"{"clients":[{"duid":"0000000700410080aa","device":{"name":"PS5-591"},"systemData":{"installedTitles":{"updatedDateTime":"2026-10-22T09:00:00Z","titles":[{"npTitleId":"ppsa01234_00","titleId":"x"},{"npTitleId":"PPSA05555"},{"npTitleId":"PPSA01234_00"},{"npTitleId":"bad id!"}]}}},{"device":{"name":"no duid"}}]}"#)).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "PS5-591");
        assert_eq!(r[0].installed.iter().map(|t| t.title_id.as_str()).collect::<Vec<_>>().len(), 2);
        assert!(
            r[0].installed.iter().any(|t| t.title_id == "PPSA01234")
                && r[0].installed.iter().any(|t| t.title_id == "PPSA05555")
        );
        assert!(r[0].installed.iter().all(|t| t.kind == "game"));
        assert_eq!(r[0].updated.as_deref(), Some("2026-10-22T09:00:00Z"));
        assert!(parse_installed(&ok(r#"{"clients":[{"duid":"d"}]}"#)).unwrap()[0].installed.is_empty());
    }

    #[test]
    fn installed_titles_carry_the_consoles_own_name_kind_and_recency() {
        let r = parse_installed(&ok(r#"{"clients":[{"duid":"d","device":{"name":"P"},"systemData":{"installedTitles":{"titles":[{"npTitleId":"PPSA1_00","comment":"Old Game","psPlatform":"PS5","displayLocationSpace":"game","size":10,"lastAccessDateTime":"2026-01-01T00:00:00Z"},{"npTitleId":"PPSA2_00","comment":"Netflix","psPlatform":"PS5","displayLocationSpace":"media","lastAccessDateTime":"2026-10-01T00:00:00Z"},{"npTitleId":"CUSA3_00","comment":"Newest","psPlatform":"PS4","displayLocationSpace":"game","lastAccessDateTime":"2026-10-22T00:00:00Z"}]}}}]}"#)).unwrap();
        let names: Vec<_> =
            r[0].installed.iter().map(|t| (t.name.as_str(), t.kind.as_str(), t.platform.as_str())).collect();
        assert_eq!(names, [("Newest", "game", "PS4"), ("Netflix", "media", "PS5"), ("Old Game", "game", "PS5")]);
        assert_eq!(r[0].installed[2].size_bytes, 10);
    }

    #[test]
    fn launch_command_is_built_like_the_original_and_answers_parse() {
        let r = launch_request("TOK", "PROSPERO", "DUID1", "PPSA01234_00");
        assert_eq!((r.method, r.url.as_str()), ("POST", COMMANDS));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["commandDetail"]["commandType"], "launchTitle");
        assert_eq!(v["commandDetail"]["parameters"]["titleIds"][0], "PPSA01234");
        assert_eq!(v["commandDetail"]["duid"], "DUID1");
        assert_eq!(r.header("authorization"), Some("Bearer TOK"));
        assert_eq!(parse_command_id(&ok(r#"{"commandId":"abc-1"}"#)).unwrap(), "abc-1");
        assert!(parse_command_id(&ok(r#"{}"#)).is_err());
        let st = parse_status(&ok(r#"{"commands":[{"commandId":"abc-1","commandStatus":"completed","commandResult":"success","actions":[{"action":{"actionType":"launchTitle","actionState":"done"}}]}]}"#)).unwrap();
        assert_eq!(
            (st.status.as_str(), st.result.as_str(), st.action.as_str(), st.action_state.as_str()),
            ("completed", "success", "launchTitle", "done")
        );
        assert!(parse_status(&ok("{}")).is_err());
    }

    #[test]
    fn title_ids_compare_without_the_suffix_or_case() {
        assert_eq!(norm_title("ppsa01234_00"), "PPSA01234");
        assert_eq!(norm_title(" PPSA01234 "), "PPSA01234");
    }

    #[test]
    fn merge_drops_duplicates() {
        let m = merge(vec![game("A", "1", None), game("a", "2", None)], vec![]);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn thumbnails_are_requested_from_the_image_service_only() {
        assert_eq!(
            thumbnail_url("https://image.api.playstation.com/a/b.jpg"),
            "https://image.api.playstation.com/a/b.jpg?w=400"
        );
        assert_eq!(
            thumbnail_url("https://image.api.playstation.com/a.jpg?x=1"),
            "https://image.api.playstation.com/a.jpg?x=1"
        );
        assert_eq!(
            thumbnail_url("https://psnobj.prod.dl.playstation.net/a.png"),
            "https://psnobj.prod.dl.playstation.net/a.png"
        );
    }

    #[test]
    fn art_comes_only_from_sonys_hosts_over_https() {
        assert!(art_url_allowed("https://image.api.playstation.com/gt5/images/a.png?w=300"));
        assert!(art_url_allowed("https://psnobj.prod.dl.playstation.net/psnobj/x.png"));
        assert!(!art_url_allowed(&format!("https://image.api.playstation.com/{}", "a".repeat(3000))));
        for bad in [
            "http://image.api.playstation.com/a.png",
            "https://evil.example/a.png",
            "https://image.api.playstation.com.evil.example/a.png",
            "https://user@image.api.playstation.com/a.png",
            "https://image.api.playstation.com:8443/a.png",
            "https://playstation.com.evil.net/a.png",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!art_url_allowed(bad), "{bad}");
        }
    }
}
