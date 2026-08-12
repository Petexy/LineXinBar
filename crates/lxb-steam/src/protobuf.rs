//! Protocol Buffers, as much of them as talking to one Steam service takes.
//!
//! Steam's authentication service speaks protobuf and nothing else: the
//! request goes up as `input_protobuf_encoded`, and what comes back is a bare
//! message with no envelope. Both directions are the *wire* format, which is
//! the small, frozen half of protobuf — a field is a varint tag saying which
//! number and which of five shapes it is, followed by the value. Nothing in
//! this crate needs the rest of it: no reflection, no `Any`, no maps, no
//! groups, no descriptors.
//!
//! So it is written out here rather than generated. A code generator would
//! bring a build script, a compiler for a second language, and the `.proto`
//! files themselves — which Valve does not ship, so they would be somebody's
//! transcription vendored into this repository and going stale on its own
//! schedule. The eight messages this crate exchanges are written as the field
//! numbers they are, beside the service that sends them, where a change to
//! Steam's own definitions shows up as a field that is simply absent rather
//! than as a build failure in a file nobody reads.
//!
//! Unknown fields are the normal case rather than an error. Steam adds fields
//! to these messages regularly, and a reader that refused a message carrying
//! one would break every time it did; [`Reader`] skips what it does not know,
//! which is what the format is designed for.

/// What a field's tag says its value is shaped like.
///
/// The five the wire format has. `StartGroup`/`EndGroup` were deprecated
/// before Steam's services existed and are not accepted: a message carrying
/// one is not one of ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Varint,
    Fixed64,
    Delimited,
    Fixed32,
}

impl Shape {
    fn of(tag: u64) -> Option<Shape> {
        match tag & 0b111 {
            0 => Some(Shape::Varint),
            1 => Some(Shape::Fixed64),
            2 => Some(Shape::Delimited),
            5 => Some(Shape::Fixed32),
            _ => None,
        }
    }
}

/// A message being built.
///
/// Fields are written in the order they are asked for, which for these
/// messages is field-number order because that is how they are written down.
/// The format does not require it and no reader depends on it.
#[derive(Debug, Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// A number, in the shape almost every number in these messages takes.
    ///
    /// Skipped when it is zero, which is not an optimisation: proto3 defines
    /// an absent field and a field set to its default as the same thing, so a
    /// zero written out is a zero the server will read back as absent anyway.
    /// Writing it is therefore bytes on the wire that say nothing.
    pub fn varint(&mut self, field: u32, value: u64) -> &mut Self {
        if value == 0 {
            return self;
        }
        self.tag(field, Shape::Varint);
        put_varint(&mut self.bytes, value);
        self
    }

    /// A signed number. Steam's enums are `int32` and one of the ones this
    /// crate sends is negative — the operating system this is running on —
    /// which on the wire is a varint sign-extended to ten bytes rather than
    /// zigzagged, because the field is `int32` and not `sint32`.
    pub fn int32(&mut self, field: u32, value: i32) -> &mut Self {
        if value == 0 {
            return self;
        }
        self.tag(field, Shape::Varint);
        put_varint(&mut self.bytes, value as i64 as u64);
        self
    }

    pub fn bool(&mut self, field: u32, value: bool) -> &mut Self {
        self.varint(field, u64::from(value))
    }

    /// A number that is always eight bytes wide. Steam spells a SteamID this
    /// way in some messages and as a varint in others; which one a field is is
    /// part of that message's definition and not a choice.
    pub fn fixed64(&mut self, field: u32, value: u64) -> &mut Self {
        if value == 0 {
            return self;
        }
        self.tag(field, Shape::Fixed64);
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// A string. Empty strings are skipped for the reason zero is.
    pub fn string(&mut self, field: u32, value: &str) -> &mut Self {
        if value.is_empty() {
            return self;
        }
        self.bytes(field, value.as_bytes())
    }

    pub fn bytes(&mut self, field: u32, value: &[u8]) -> &mut Self {
        if value.is_empty() {
            return self;
        }
        self.tag(field, Shape::Delimited);
        put_varint(&mut self.bytes, value.len() as u64);
        self.bytes.extend_from_slice(value);
        self
    }

    /// A message inside a message, which on the wire is a string of its bytes.
    ///
    /// An empty one is still written: an embedded message that is *present and
    /// empty* differs from an absent one for a reader that asks whether the
    /// field was there, and the device details this crate sends are one Steam
    /// looks for rather than reads.
    pub fn message(&mut self, field: u32, value: Writer) -> &mut Self {
        let inner = value.finish();
        self.tag(field, Shape::Delimited);
        put_varint(&mut self.bytes, inner.len() as u64);
        self.bytes.extend_from_slice(&inner);
        self
    }

    fn tag(&mut self, field: u32, shape: Shape) {
        let shape = match shape {
            Shape::Varint => 0,
            Shape::Fixed64 => 1,
            Shape::Delimited => 2,
            Shape::Fixed32 => 5,
        };
        put_varint(&mut self.bytes, (u64::from(field) << 3) | shape);
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// One field, as it was read off the wire.
///
/// Owns its bytes. The alternative — lending them out of the buffer the
/// message arrived in — is what a protobuf library does, and is the right
/// trade when messages are large or read in a loop. Neither is true here: the
/// largest message this crate reads is a few hundred bytes, one is read per
/// call to Steam, and a borrowed field cannot be handed back out of the
/// function that made the call without the buffer being kept alive by hand.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A varint or a fixed-width integer. Which of the three it was on the
    /// wire is not kept: the field's own definition says how wide it is, and
    /// no message here has a field that could be either.
    Number(u64),
    /// Four bytes, kept as they arrived. Read as a `float` by the one field in
    /// any of this that is one — how long to wait between one poll of a
    /// sign-in and the next — and as a number by anything else that turns up
    /// in this shape later.
    Fixed32(u32),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(value) => Some(*value),
            Value::Fixed32(value) => Some(u64::from(*value)),
            Value::Bytes(_) => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        self.as_u64().map(|value| value != 0)
    }

    /// A signed number, read back the way [`Writer::int32`] wrote it.
    pub fn as_i32(&self) -> Option<i32> {
        self.as_u64().map(|value| value as u32 as i32)
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Fixed32(bits) => Some(f32::from_bits(*bits)),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    /// A string field. Steam sends valid UTF-8 in every one of these; a field
    /// that is not is treated as absent rather than as a reason to fail the
    /// whole message, because one unreadable display name must not cost the
    /// user their sign-in.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()?).ok()
    }

    /// This field read as a message of its own — which is what an embedded
    /// message is on the wire, and what a nested field is asked for through.
    pub fn as_message(&self) -> Vec<(u32, Value)> {
        self.as_bytes().map(read).unwrap_or_default()
    }
}

/// Read a message into its fields, in the order they arrived.
///
/// Repeated fields are why this is a list rather than a map: a message can
/// carry the same field number several times — the ways a sign-in may be
/// confirmed arrive that way — and a map would keep one of them.
pub fn read(bytes: &[u8]) -> Vec<(u32, Value)> {
    let mut reader = Reader { bytes, at: 0 };
    let mut fields = Vec::new();
    while let Some(field) = reader.next() {
        fields.push(field);
    }
    fields
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// The next field, as a number and a value. `None` at the end of the
    /// message, and also at the first byte that is not a field — a truncated
    /// answer stops being read where it stops making sense, and what was read
    /// before that point still stands.
    fn next(&mut self) -> Option<(u32, Value)> {
        let tag = self.varint()?;
        let field = (tag >> 3) as u32;
        if field == 0 {
            return None;
        }
        let value = match Shape::of(tag)? {
            Shape::Varint => Value::Number(self.varint()?),
            Shape::Fixed64 => Value::Number(u64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            Shape::Fixed32 => Value::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().ok()?)),
            Shape::Delimited => {
                let length = self.varint()? as usize;
                Value::Bytes(self.take(length)?.to_vec())
            }
        };
        Some((field, value))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for step in 0..10 {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << (step * 7);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        // Eleven bytes is not a 64-bit varint, so this is not a message.
        None
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(length)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }
}

/// One message, as the fields it was read into.
pub type Message = Vec<(u32, Value)>;

/// The first field of `message` with this number, if it carries one.
pub fn field(message: &Message, number: u32) -> Option<&Value> {
    message
        .iter()
        .find(|(field, _)| *field == number)
        .map(|(_, value)| value)
}

/// The same, as the string it is — the commonest thing asked of one of these
/// messages, and worth not spelling out at every call.
pub fn text(message: &Message, number: u32) -> Option<String> {
    field(message, number)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// The same, as a number, with an absent field reading as the zero it means.
pub fn number(message: &Message, number: u32) -> u64 {
    field(message, number)
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

/// Every field of `message` with this number, for the ones that repeat.
pub fn every(message: &Message, number: u32) -> Vec<&Value> {
    message
        .iter()
        .filter(|(field, _)| *field == number)
        .map(|(_, value)| value)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shapes this crate actually sends, written and read back.
    #[test]
    fn a_message_survives_the_round_trip() {
        let mut inner = Writer::new();
        inner.string(1, "LineXinBar").int32(3, -203);

        let mut message = Writer::new();
        message
            .string(1, "a device")
            .varint(2, 1)
            .message(3, inner)
            .fixed64(4, 0x0110_0001_0000_0001)
            .bool(5, true);

        let bytes = message.finish();
        let fields = read(&bytes);

        assert_eq!(text(&fields, 1).as_deref(), Some("a device"));
        assert_eq!(number(&fields, 2), 1);
        assert_eq!(number(&fields, 4), 0x0110_0001_0000_0001);
        assert_eq!(field(&fields, 5).and_then(Value::as_bool), Some(true));

        let inner = field(&fields, 3).expect("nested").as_message();
        assert_eq!(text(&inner, 1).as_deref(), Some("LineXinBar"));
        assert_eq!(field(&inner, 3).and_then(Value::as_i32), Some(-203));
    }

    /// A default is an absent field, so writing one is bytes that say nothing.
    #[test]
    fn defaults_are_left_off_the_wire() {
        let mut message = Writer::new();
        message
            .varint(1, 0)
            .string(2, "")
            .bytes(3, &[])
            .bool(4, false)
            .int32(5, 0);
        assert!(message.finish().is_empty());

        // An embedded message is the exception: present-and-empty is a fact
        // about the message, and Steam looks for the field rather than in it.
        let mut message = Writer::new();
        message.message(9, Writer::new());
        assert_eq!(message.finish(), vec![0x4a, 0x00]);
    }

    /// A negative `int32` is sign-extended rather than zigzagged: the field is
    /// `int32`, and a reader of `sint32` would decode this as a different
    /// number entirely.
    #[test]
    fn a_negative_int32_is_ten_bytes_of_sign_extension() {
        let mut message = Writer::new();
        message.int32(3, -203);
        let bytes = message.finish();
        assert_eq!(bytes.len(), 1 + 10, "tag and ten varint bytes");
        assert_eq!(field(&read(&bytes), 3).and_then(Value::as_i32), Some(-203));
    }

    /// A field this crate has never heard of is skipped, whatever shape it is
    /// — which is the normal case, because Steam adds them.
    #[test]
    fn unknown_fields_are_stepped_over() {
        let mut message = Writer::new();
        message
            .varint(1, 7)
            .string(400, "a field written after this was")
            .fixed64(401, 12)
            .string(2, "the one that was wanted");

        let fields = read(&message.finish());
        assert_eq!(text(&fields, 2).as_deref(), Some("the one that was wanted"));
    }

    /// A field that repeats is every one of them, not the last.
    #[test]
    fn repeated_fields_all_arrive() {
        let mut message = Writer::new();
        for confirmation in [2u64, 4] {
            let mut allowed = Writer::new();
            allowed.varint(1, confirmation);
            message.message(4, allowed);
        }
        let fields = read(&message.finish());

        let kinds: Vec<u64> = every(&fields, 4)
            .into_iter()
            .map(|allowed| number(&allowed.as_message(), 1))
            .collect();
        assert_eq!(kinds, vec![2, 4]);
    }

    /// A truncated answer is read as far as it makes sense and no further,
    /// rather than being thrown away or read off the end of the buffer.
    #[test]
    fn a_truncated_message_keeps_what_it_managed_to_say() {
        let mut message = Writer::new();
        message.string(1, "first").string(2, "second");
        let bytes = message.finish();

        for cut in 1..bytes.len() {
            let fields = read(&bytes[..cut]);
            assert!(fields.len() <= 2);
            if let Some(first) = text(&fields, 1) {
                assert_eq!(first, "first");
            }
        }
    }

    /// The interval between polls is a `float`, which is the one fixed-32
    /// field in any of this.
    #[test]
    fn a_float_reads_back_as_itself() {
        let bytes = [0x1d, 0x00, 0x00, 0xa0, 0x40];
        assert_eq!(field(&read(&bytes), 3).and_then(Value::as_f32), Some(5.0));
    }

    /// An absent number reads as the zero proto3 says it is, so a caller never
    /// has to tell "not sent" from "sent as nothing".
    #[test]
    fn an_absent_field_reads_as_its_default() {
        let fields = read(&[]);
        assert_eq!(number(&fields, 1), 0);
        assert_eq!(text(&fields, 1), None);
    }
}
