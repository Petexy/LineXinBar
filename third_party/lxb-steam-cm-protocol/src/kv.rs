/// Minimal binary KeyValues representation used by Steam PICS/app schemas.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum KVValue {
    Nested(Vec<(String, KVValue)>),
    Str(String),
    Int(i32),
    Uint64(u64),
    Other,
}

impl KVValue {
    pub(crate) fn as_nested(&self) -> Option<&Vec<(String, KVValue)>> {
        if let KVValue::Nested(n) = self {
            Some(n)
        } else {
            None
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        if let KVValue::Str(s) = self {
            Some(s)
        } else {
            None
        }
    }

    pub(crate) fn as_u32(&self) -> Option<u32> {
        match self {
            KVValue::Int(i) => (*i >= 0).then_some(*i as u32),
            KVValue::Uint64(i) => u32::try_from(*i).ok(),
            KVValue::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&KVValue> {
        self.as_nested()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            Some(b)
        } else {
            None
        }
    }

    fn read_i32_le(&mut self) -> Option<i32> {
        if self.remaining() < 4 {
            return None;
        }
        let bytes = &self.data[self.pos..self.pos + 4];
        self.pos += 4;
        Some(i32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64_le(&mut self) -> Option<u64> {
        if self.remaining() < 8 {
            return None;
        }
        let bytes = &self.data[self.pos..self.pos + 8];
        self.pos += 8;
        Some(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn skip(&mut self, n: usize) -> bool {
        if self.remaining() < n {
            return false;
        }
        self.pos += n;
        true
    }

    fn read_null_string(&mut self) -> Option<String> {
        let start = self.pos;
        while self.pos < self.data.len() {
            if self.data[self.pos] == 0 {
                let s = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
                self.pos += 1;
                return Some(s);
            }
            self.pos += 1;
        }
        None
    }

    fn skip_wstring(&mut self) -> bool {
        let len = self.read_i32_le().unwrap_or(-1);
        if len < 0 {
            return false;
        }
        self.skip(len as usize * 2)
    }
}

fn parse_children(r: &mut Reader<'_>) -> Option<Vec<(String, KVValue)>> {
    let mut children = Vec::new();
    loop {
        let type_byte = r.read_u8()?;
        if type_byte == 0x08 || type_byte == 0x0b {
            break;
        }

        let key = r.read_null_string()?;
        let value = match type_byte {
            0x00 => KVValue::Nested(parse_children(r)?),
            0x01 => KVValue::Str(r.read_null_string()?),
            0x02 | 0x04 | 0x06 => KVValue::Int(r.read_i32_le()?),
            0x03 => {
                if !r.skip(4) {
                    return None;
                }
                KVValue::Other
            }
            0x05 => {
                if !r.skip_wstring() {
                    return None;
                }
                KVValue::Other
            }
            0x07 | 0x0a => KVValue::Uint64(r.read_u64_le()?),
            _ => return None,
        };
        children.push((key, value));
    }
    Some(children)
}

fn parse_binary_kv_at(data: &[u8], offset: usize) -> Option<KVValue> {
    let mut r = Reader::new(data);
    if !r.skip(offset) {
        return None;
    }
    let type_byte = r.read_u8()?;
    if type_byte != 0x00 {
        return None;
    }
    let _key = r.read_null_string()?;
    let children = parse_children(&mut r)?;
    Some(KVValue::Nested(children))
}

pub(crate) fn parse_binary_kv(data: &[u8]) -> Option<KVValue> {
    parse_binary_kv_at(data, 0).or_else(|| {
        // PICS package buffers prepend a little-endian version/header before
        // the Binary KV root object.
        if data.len() >= 5 && data[..4] == [1, 0, 0, 0] {
            parse_binary_kv_at(data, 4)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_string_and_int_values() {
        let data = [
            0x00, 0x00, // root
            0x00, b'a', b'p', b'p', b'i', b'd', b's', 0x00, 0x02, b'0', 0x00, 10, 0, 0, 0, 0x01,
            b'1', 0x00, b'2', b'0', 0x00, 0x08, 0x08,
        ];

        let root = parse_binary_kv(&data).expect("binary kv should parse");
        let appids = root.get("appids").expect("appids node");
        let values: Vec<u32> = appids
            .as_nested()
            .unwrap()
            .iter()
            .filter_map(|(_, value)| value.as_u32())
            .collect();
        assert_eq!(values, vec![10, 20]);
    }

    #[test]
    fn parses_pics_package_version_header() {
        let data = [
            0x01, 0x00, 0x00, 0x00, // PICS Binary KV version/header
            0x00, b'3', b'2', 0x00, // package root
            0x02, b'p', b'a', b'c', b'k', b'a', b'g', b'e', b'i', b'd', 0x00, 32, 0, 0, 0, 0x00,
            b'a', b'p', b'p', b'i', b'd', b's', 0x00, 0x02, b'0', 0x00, 32, 0, 0, 0, 0x08, 0x08,
        ];

        let root = parse_binary_kv(&data).expect("version-prefixed binary kv should parse");
        assert_eq!(root.get("packageid").and_then(KVValue::as_u32), Some(32));
        let appids = root.get("appids").expect("appids node");
        let values: Vec<u32> = appids
            .as_nested()
            .unwrap()
            .iter()
            .filter_map(|(_, value)| value.as_u32())
            .collect();
        assert_eq!(values, vec![32]);
    }
}
