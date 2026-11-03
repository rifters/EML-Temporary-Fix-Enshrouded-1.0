//! Chat lines understood by the server's chat channel (`!m182 <key> ...`). File contents travel
//! as hexadecimal text so the only characters typed are letters, digits and a few separators.

pub const MIN_LIMIT: usize = 80;
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 900;

pub const PREFIX: &str = "!m182";

fn head(key: &str, command: &str, file: &str) -> String {
    format!("{PREFIX} {key} {command} {file} ")
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn cut(text: &str, from: usize, size: usize) -> usize {
    let rest = &text[from..];
    if rest.len() <= size {
        return text.len();
    }
    let mut end = size;
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    let ok = |at: usize| {
        let before = rest[..at].chars().next_back();
        let after = rest[at..].chars().next();
        at > 0 && !before.is_some_and(char::is_whitespace) && !after.is_some_and(char::is_whitespace)
    };
    let mut at = end;
    while at > 0 && !ok(at) {
        at -= 1;
        while at > 0 && !rest.is_char_boundary(at) {
            at -= 1;
        }
    }
    if at == 0 {
        at = end;
    }
    from + at
}

/// Every chat line needed to write `body` (compact JSON) into `file` on the server.
pub fn compose(key: &str, file: &str, body: &str, limit: usize) -> Vec<String> {
    let limit = limit.clamp(MIN_LIMIT, MAX_LIMIT);
    let single = format!("{}{}", head(key, "setx", file), hex(body));
    if single.len() <= limit {
        return vec![single];
    }
    let room = (limit.saturating_sub(head(key, "addx", file).len()) / 2).max(8);
    let mut lines = Vec::new();
    let mut from = 0;
    while from < body.len() {
        let to = cut(body, from, room);
        let command = if from == 0 { "putx" } else { "addx" };
        lines.push(format!("{}{}", head(key, command, file), hex(&body[from..to])));
        from = to;
    }
    let end = format!("{PREFIX} {key} endx {file}");
    lines.push(end.clone());
    lines.push(end);
    lines
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64[n as usize & 63] as char);
        }
    }
    out
}

/// Same as `compose`, but the body is deflated and written in base64 (about 8 times fewer chat lines).
pub fn compose_z(key: &str, file: &str, body: &str, limit: usize) -> Vec<String> {
    let limit = limit.clamp(MIN_LIMIT, MAX_LIMIT);
    let packed = b64(&miniz_oxide::deflate::compress_to_vec(body.as_bytes(), 9));
    let single = format!("{}{}", head(key, "setz", file), packed);
    if single.len() <= limit {
        return vec![single];
    }
    let room = limit.saturating_sub(head(key, "addz", file).len()).max(8);
    let mut lines = Vec::new();
    for (i, part) in packed.as_bytes().chunks(room).enumerate() {
        let command = if i == 0 { "putz" } else { "addz" };
        lines.push(format!("{}{}", head(key, command, file), std::str::from_utf8(part).unwrap_or("")));
    }
    let end = format!("{PREFIX} {key} endz {file}");
    lines.push(end.clone());
    lines.push(end);
    lines
}

/// The shorter of the two encodings (the plain one wins ties so small messages stay readable).
pub fn compose_best(key: &str, file: &str, body: &str, limit: usize) -> Vec<String> {
    let plain = compose(key, file, body, limit);
    let packed = compose_z(key, file, body, limit);
    if packed.len() < plain.len() { packed } else { plain }
}

pub fn pvp(key: &str, on: bool) -> String {
    format!("{PREFIX} {key} pvp {}", if on { "on" } else { "off" })
}

pub fn friendly_fire(key: &str, on: bool) -> String {
    format!("{PREFIX} {key} friendlyfire {}", if on { "on" } else { "off" })
}

pub fn ping(key: &str) -> String {
    format!("{PREFIX} {key} ping")
}

pub fn players(key: &str, seq: u32) -> String {
    format!("{PREFIX} {key} players {seq}")
}

pub fn teams(key: &str, seq: u32) -> String {
    format!("{PREFIX} {key} teams {seq}")
}

pub fn valid_key(key: &str) -> bool {
    key.len() >= 6 && !key.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(text: &str) -> String {
        let bytes: Vec<u8> = (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect();
        String::from_utf8(bytes).unwrap()
    }

    fn rebuild(lines: &[String]) -> String {
        let mut out = String::new();
        for line in lines {
            let mut parts = line.splitn(5, ' ');
            let (_, _, command, _file) = (parts.next(), parts.next(), parts.next(), parts.next());
            if matches!(command, Some("putx" | "addx" | "setx")) {
                out.push_str(&unhex(parts.next().unwrap()));
            }
        }
        out
    }

    #[test]
    fn short_bodies_go_in_one_line() {
        let lines = compose("secret-key", "m182_spawn.request", r#"{"template":"x"}"#, 200);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("!m182 secret-key setx m182_spawn.request "));
        assert_eq!(rebuild(&lines), r#"{"template":"x"}"#);
        assert!(lines[0].chars().all(|c| c.is_ascii_alphanumeric() || " !-_.".contains(c)));
    }

    #[test]
    fn long_bodies_are_split_and_rebuilt_exactly() {
        let body = serde_json::json!({"zones": (0..12).map(|i| serde_json::json!({"name": format!("Zona número {i} ñandú"), "x": i * 100, "z": -i * 33, "radius": 20})).collect::<Vec<_>>()}).to_string();
        for limit in [80usize, 120, 200, 333, 900] {
            let lines = compose("secret-key", "m182_zones.json", &body, limit);
            assert!(lines.len() >= 2 || body.len() < limit);
            assert!(lines.iter().all(|l| l.len() <= limit.clamp(MIN_LIMIT, MAX_LIMIT)), "limit {limit}: {:?}", lines.iter().map(|l| l.len()).collect::<Vec<_>>());
            assert!(lines[0].contains(" putx ") || lines.len() == 1);
            assert!(lines.last().unwrap().ends_with(" endx m182_zones.json") || lines.len() == 1);
            assert_eq!(rebuild(&lines), body, "limit {limit}");
            for l in &lines {
                let payload = l.splitn(5, ' ').nth(4).unwrap_or("");
                assert!(payload.is_empty() || (!payload.starts_with(' ') && !payload.ends_with(' ')), "{l:?}");
            }
        }
    }

    #[test]
    fn small_commands() {
        assert_eq!(pvp("abcdefg", true), "!m182 abcdefg pvp on");
        assert_eq!(pvp("abcdefg", false), "!m182 abcdefg pvp off");
        assert_eq!(friendly_fire("abcdefg", true), "!m182 abcdefg friendlyfire on");
        assert_eq!(friendly_fire("abcdefg", false), "!m182 abcdefg friendlyfire off");
        assert_eq!(ping("abcdefg"), "!m182 abcdefg ping");
        assert_eq!(players("abcdefg", 7), "!m182 abcdefg players 7");
        assert!(valid_key("abcdef") && !valid_key("abc") && !valid_key("abc def"));
    }

    #[test]
    fn packed_bodies_use_far_fewer_lines() {
        let body = serde_json::json!({"spawners": (0..160).map(|i| serde_json::json!({"id": format!("s17929{i:05}"), "name": format!("Raid {i}"), "x": i * 10, "z": -i * 7, "waves": [{"template": "Enemy_Wolf", "level": 20, "count": 5}]})).collect::<Vec<_>>()}).to_string();
        let plain = compose("secret-key", "m182_spawners.json", &body, 200);
        let packed = compose_z("secret-key", "m182_spawners.json", &body, 200);
        assert!(packed.len() * 4 < plain.len(), "{} vs {}", packed.len(), plain.len());
        assert!(packed.iter().all(|l| l.len() <= 200));
        assert!(packed.iter().all(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || " !-_.".contains(c))));
        assert_eq!(compose_best("secret-key", "m182_spawners.json", &body, 200), packed);
    }

    fn unpack(lines: &[String]) -> String {
        let mut packed = String::new();
        for l in lines {
            let mut parts = l.splitn(5, ' ');
            let (_, _, command, _file) = (parts.next(), parts.next(), parts.next(), parts.next());
            if matches!(command, Some("putz" | "addz" | "setz")) {
                packed.push_str(parts.next().unwrap());
            }
        }
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in packed.bytes() {
            let v = B64.iter().position(|b| *b == c).unwrap() as u32;
            acc = acc << 6 | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        String::from_utf8(miniz_oxide::inflate::decompress_to_vec(&out).unwrap()).unwrap()
    }

    #[test]
    fn packed_bodies_rebuild_exactly() {
        for body in [r#"{"a":1}"#.to_string(), serde_json::json!({"zones": (0..300).map(|i| serde_json::json!({"name": format!("Zone {i}"), "x": i * 13 % 977, "z": -(i * 31 % 811)})).collect::<Vec<_>>()}).to_string()] {
            for limit in [80usize, 200, 900] {
                let lines = compose_z("secret-key", "m182_zones.json", &body, limit);
                assert!(lines.iter().all(|l| l.len() <= limit));
                assert_eq!(unpack(&lines), body, "limit {limit}");
            }
        }
    }
}
