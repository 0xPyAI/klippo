//! Just enough protobuf for YouTube's streaming messages: read the fields of
//! a message and write new ones. Unknown fields are kept as raw values so a
//! template message can be sent back with a few fields replaced.

/// One field value. Length-delimited values are left undecoded (strings,
/// bytes and nested messages look the same on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Varint(u64),
    Fixed64([u8; 8]),
    Bytes(Vec<u8>),
    Fixed32([u8; 4]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub number: u32,
    pub value: Value,
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

/// All fields of a message, or None when the bytes are not a message.
pub fn parse(buf: &[u8]) -> Option<Vec<Field>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < buf.len() {
        let key = read_varint(buf, &mut pos)?;
        let number = u32::try_from(key >> 3).ok().filter(|n| *n > 0)?;
        let value = match key & 7 {
            0 => Value::Varint(read_varint(buf, &mut pos)?),
            1 => {
                let b: [u8; 8] = buf.get(pos..pos + 8)?.try_into().ok()?;
                pos += 8;
                Value::Fixed64(b)
            }
            2 => {
                let len = usize::try_from(read_varint(buf, &mut pos)?).ok()?;
                let bytes = buf.get(pos..pos.checked_add(len)?)?.to_vec();
                pos += len;
                Value::Bytes(bytes)
            }
            5 => {
                let b: [u8; 4] = buf.get(pos..pos + 4)?.try_into().ok()?;
                pos += 4;
                Value::Fixed32(b)
            }
            _ => return None,
        };
        out.push(Field { number, value });
    }
    Some(out)
}

/// First varint field `number`.
pub fn varint(fields: &[Field], number: u32) -> Option<u64> {
    fields.iter().find_map(|f| match f.value {
        Value::Varint(v) if f.number == number => Some(v),
        _ => None,
    })
}

/// First length-delimited field `number`.
pub fn bytes(fields: &[Field], number: u32) -> Option<&[u8]> {
    fields.iter().find_map(|f| match &f.value {
        Value::Bytes(b) if f.number == number => Some(b.as_slice()),
        _ => None,
    })
}

/// First length-delimited field `number`, decoded as a message.
pub fn message(fields: &[Field], number: u32) -> Option<Vec<Field>> {
    bytes(fields, number).and_then(parse)
}

/// Builds a message field by field.
#[derive(Debug, Default, Clone)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    fn raw_varint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    pub fn varint(&mut self, number: u32, v: u64) -> &mut Self {
        self.raw_varint(u64::from(number) << 3);
        self.raw_varint(v);
        self
    }

    pub fn bytes(&mut self, number: u32, b: &[u8]) -> &mut Self {
        self.raw_varint((u64::from(number) << 3) | 2);
        self.raw_varint(b.len() as u64);
        self.buf.extend_from_slice(b);
        self
    }

    pub fn field(&mut self, f: &Field) -> &mut Self {
        match &f.value {
            Value::Varint(v) => self.varint(f.number, *v),
            Value::Bytes(b) => self.bytes(f.number, b),
            Value::Fixed64(b) => {
                self.raw_varint((u64::from(f.number) << 3) | 1);
                self.buf.extend_from_slice(b);
                self
            }
            Value::Fixed32(b) => {
                self.raw_varint((u64::from(f.number) << 3) | 5);
                self.buf.extend_from_slice(b);
                self
            }
        }
    }

    pub fn finish(&self) -> Vec<u8> {
        self.buf.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_unknown_fields() {
        let mut inner = Writer::new();
        inner.varint(1, 140).bytes(3, b"drc");
        let mut w = Writer::new();
        w.varint(28, 123_456).bytes(2, &inner.finish()).field(&Field { number: 9, value: Value::Fixed32([1, 2, 3, 4]) });
        let msg = w.finish();
        let fields = parse(&msg).unwrap();
        assert_eq!(varint(&fields, 28), Some(123_456));
        let nested = message(&fields, 2).unwrap();
        assert_eq!(varint(&nested, 1), Some(140));
        assert_eq!(bytes(&nested, 3), Some(&b"drc"[..]));
        let mut again = Writer::new();
        for f in &fields {
            again.field(f);
        }
        assert_eq!(again.finish(), msg);
        assert!(parse(&[0x0A, 0x05, 1]).is_none(), "length past the end");
    }
}
