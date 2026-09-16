use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use serde::Deserialize;

#[derive(Deserialize)]
struct RefreshTokenClaims {
    sub: SubClaim,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SubClaim {
    String(String),
    Number(u64),
}

pub fn steamid_from_refresh_token(token: &str) -> Option<u64> {
    let mut segments = token.split('.');
    let _header = segments.next()?;
    let payload = segments.next()?;
    let _signature = segments.next()?;

    if segments.next().is_some() || payload.is_empty() {
        return None;
    }

    let decoded_payload = decode_jwt_segment(payload)?;
    let claims: RefreshTokenClaims = serde_json::from_slice(&decoded_payload).ok()?;

    match claims.sub {
        SubClaim::String(value) => parse_steamid(&value),
        SubClaim::Number(value) => Some(value),
    }
}

fn decode_jwt_segment(segment: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(segment)
        .or_else(|_| URL_SAFE.decode(segment))
        .ok()
}

fn parse_steamid(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::steamid_from_refresh_token;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn extracts_steamid_from_string_sub_claim() {
        let token = make_token(r#"{"sub":"76561198000000000","aud":["web"]}"#);

        assert_eq!(
            steamid_from_refresh_token(&token),
            Some(76_561_198_000_000_000)
        );
    }

    #[test]
    fn extracts_steamid_from_numeric_sub_claim() {
        let token = make_token(r#"{"sub":76561198000000000}"#);

        assert_eq!(
            steamid_from_refresh_token(&token),
            Some(76_561_198_000_000_000)
        );
    }

    #[test]
    fn rejects_token_with_wrong_segment_count() {
        assert_eq!(steamid_from_refresh_token("header.payload"), None,);
        assert_eq!(
            steamid_from_refresh_token("header.payload.signature.extra"),
            None,
        );
    }

    #[test]
    fn rejects_invalid_base64_payload() {
        assert_eq!(steamid_from_refresh_token("header.@@@.signature"), None);
    }

    #[test]
    fn rejects_non_json_payload() {
        let token = make_token("not-json");

        assert_eq!(steamid_from_refresh_token(&token), None);
    }

    #[test]
    fn rejects_missing_sub_claim() {
        let token = make_token(r#"{"iss":"steam"}"#);

        assert_eq!(steamid_from_refresh_token(&token), None);
    }

    #[test]
    fn rejects_non_numeric_string_sub_claim() {
        let token = make_token(r#"{"sub":"steam-user"}"#);

        assert_eq!(steamid_from_refresh_token(&token), None);
    }

    #[test]
    fn rejects_out_of_range_string_sub_claim() {
        let token = make_token(r#"{"sub":"18446744073709551616"}"#);

        assert_eq!(steamid_from_refresh_token(&token), None);
    }

    fn make_token(payload_json: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(payload_json);
        format!("{header}.{payload}.signature")
    }
}
