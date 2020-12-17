use ethereum_types::{H160, U256};

use crate::types::Type;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Value {
    Uint(U256, usize),
    Int(U256, usize),
    Address(H160),
    Bool(bool),
    FixedBytes(Vec<u8>),
    FixedArray(Vec<Value>, Type),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<Value>, Type),
    Tuple(Vec<Value>),
}

impl Value {
    pub fn decode_from_slice(bs: &[u8], tys: &Vec<Type>) -> Result<Vec<Value>, String> {
        tys.iter()
            .try_fold((vec![], 0), |(mut values, at), ty| {
                let (value, consumed) = Self::decode(bs, ty, 0, at)?;
                values.push(value);

                Ok((values, at + consumed))
            })
            .map(|(values, _)| values)
    }

    pub fn encode(values: &Vec<Self>) -> Vec<u8> {
        let mut buf = vec![];
        let mut alloc_queue = std::collections::VecDeque::new();

        for value in values {
            match value {
                Value::Uint(i, _) | Value::Int(i, _) => {
                    let start = buf.len();
                    buf.resize(buf.len() + 32, 0);

                    i.to_big_endian(&mut buf[start..(start + 32)]);
                }

                Value::Address(addr) => {
                    let start = buf.len();
                    buf.resize(buf.len() + 32, 0);

                    // big-endian, as if it were a uint160.
                    buf[(start + 12)..(start + 32)].copy_from_slice(addr.as_fixed_bytes());
                }

                Value::Bool(b) => {
                    let start = buf.len();
                    buf.resize(buf.len() + 32, 0);

                    if *b {
                        buf[start + 31] = 1;
                    }
                }

                Value::FixedBytes(bytes) => {
                    let start = buf.len();
                    buf.resize(buf.len() + 32, 0);

                    buf[start..(start + bytes.len())].copy_from_slice(&bytes);
                }

                Value::FixedArray(values, ty) => {
                    if ty.is_dynamic() {
                        alloc_queue.push_back((buf.len(), value));
                        buf.resize(buf.len() + 32, 0);
                    } else {
                        buf.extend(Self::encode(values));
                    }
                }

                Value::String(_) | Value::Bytes(_) | Value::Array(_, _) => {
                    alloc_queue.push_back((buf.len(), value));
                    buf.resize(buf.len() + 32, 0);
                }

                Value::Tuple(_) => todo!(),
            };
        }

        let mut alloc_offset = buf.len();

        while let Some((at, value)) = alloc_queue.pop_front() {
            U256::from(alloc_offset).to_big_endian(&mut buf[at..(at + 32)]);

            match value {
                Value::String(s) => {
                    alloc_offset = Self::encode_bytes(&mut buf, s.as_bytes(), alloc_offset);
                }

                Value::Bytes(bytes) => {
                    alloc_offset = Self::encode_bytes(&mut buf, bytes, alloc_offset);
                }

                Value::Array(values, _) => {
                    buf.resize(buf.len() + 32, 0);

                    // write array length
                    U256::from(values.len())
                        .to_big_endian(&mut buf[alloc_offset..(alloc_offset + 32)]);
                    alloc_offset += 32;

                    // write array values
                    let bytes = Self::encode(values);
                    alloc_offset += bytes.len();
                    buf.extend(bytes);
                }

                Value::FixedArray(values, _) => {
                    // write array values
                    let bytes = Self::encode(values);
                    alloc_offset += bytes.len();
                    buf.extend(bytes);
                }

                _ => panic!(format!(
                    "value of fixed size type {:?} in dynamic alloc area",
                    value
                )),
            };
        }

        buf
    }

    pub fn type_of(&self) -> Type {
        match self {
            Value::Uint(_, size) => Type::Uint(*size),
            Value::Int(_, size) => Type::Int(*size),
            Value::Address(_) => Type::Address,
            Value::Bool(_) => Type::Bool,
            Value::FixedBytes(bytes) => Type::FixedBytes(bytes.len()),
            Value::FixedArray(values, ty) => Type::FixedArray(Box::new(ty.clone()), values.len()),
            Value::String(_) => Type::String,
            Value::Bytes(_) => Type::Bytes,
            Value::Array(_, ty) => Type::Array(Box::new(ty.clone())),
            Value::Tuple(_) => todo!(),
        }
    }

    fn decode(bs: &[u8], ty: &Type, base_addr: usize, at: usize) -> Result<(Value, usize), String> {
        let dec = match ty {
            Type::Uint(size) => {
                let at = base_addr + at;
                let uint = U256::from_big_endian(&bs[at..(at + 32)]);

                Ok((Value::Uint(uint, *size), 32))
            }

            Type::Int(size) => {
                let at = base_addr + at;
                let uint = U256::from_big_endian(&bs[at..(at + 32)]);

                Ok((Value::Int(uint, *size), 32))
            }

            Type::Address => {
                let at = base_addr + at;
                // big-endian, same as if it were a uint160.
                let addr = H160::from_slice(&bs[(at + 12)..(at + 32)]);

                Ok((Value::Address(addr), 32))
            }

            Type::Bool => {
                let at = base_addr + at;
                let b = U256::from_big_endian(&bs[at..(at + 32)]) == U256::one();

                Ok((Value::Bool(b), 32))
            }

            Type::FixedBytes(size) => {
                let at = base_addr + at;
                let bv = bs[at..(at + size)].to_vec();

                Ok((Value::FixedBytes(bv), Self::padded32_size(*size)))
            }

            Type::FixedArray(ty, size) => {
                let (base_addr, at) = if ty.is_dynamic() {
                    // For fixed arrays of types that are dynamic, we just jump
                    // to the offset location and decode from there.
                    let offset = U256::from_big_endian(&bs[at..(at + 32)]).as_usize();

                    (base_addr + offset, 0)
                } else {
                    // There's no need to change the addressing because fixed arrays
                    // will consume input by calling decode recursively and addressing
                    // will be computed correctly inside those calls.
                    (base_addr, at)
                };

                (0..(*size))
                    .try_fold((vec![], 0), |(mut values, total_consumed), _| {
                        let (value, consumed) =
                            Self::decode(bs, ty, base_addr, at + total_consumed)?;

                        values.push(value);

                        Ok((values, total_consumed + consumed))
                    })
                    .map(|(values, consumed)| {
                        let consumed = if ty.is_dynamic() { 32 } else { consumed };

                        (Value::FixedArray(values, *ty.clone()), consumed)
                    })
            }

            Type::String => {
                let at = base_addr + at;
                let (bytes_value, consumed) = Self::decode(bs, &Type::Bytes, base_addr, at)?;

                let bytes = if let Value::Bytes(bytes) = bytes_value {
                    bytes
                } else {
                    // should always be Value::Bytes
                    unreachable!();
                };

                let s = String::from_utf8(bytes).map_err(|e| e.to_string())?;

                Ok((Value::String(s), consumed))
            }

            Type::Bytes => {
                let at = base_addr + at;
                let offset = U256::from_big_endian(&bs[at..(at + 32)]).as_usize();

                let at = base_addr + offset;
                let bytes_len = U256::from_big_endian(&bs[at..(at + 32)]).as_usize();

                let at = at + 32;
                let bytes = bs[at..(at + bytes_len)].to_vec();

                // consumes only the first 32 bytes, i.e. the offset pointer
                Ok((Value::Bytes(bytes), 32))
            }

            Type::Array(ty) => {
                let at = base_addr + at;
                let offset = U256::from_big_endian(&bs[at..(at + 32)]).as_usize();

                let at = base_addr + offset;
                let array_len = U256::from_big_endian(&bs[at..(at + 32)]).as_usize();

                let (arr, _) = Self::decode(bs, &Type::FixedArray(ty.clone(), array_len), at, 32)?;

                let values = if let Value::FixedArray(values, _) = arr {
                    values
                } else {
                    // should always be Value::FixedArray
                    unreachable!();
                };

                Ok((Value::Array(values, *ty.clone()), 32))
            }

            Type::Tuple(_) => todo!(),
        };

        dec
    }

    fn encode_bytes(buf: &mut Vec<u8>, bytes: &[u8], mut alloc_offset: usize) -> usize {
        let padded_bytes_len = Self::padded32_size(bytes.len());
        buf.resize(buf.len() + 32 + padded_bytes_len, 0);

        // write bytes size
        U256::from(bytes.len()).to_big_endian(&mut buf[alloc_offset..(alloc_offset + 32)]);
        alloc_offset += 32;

        // write bytes
        buf[alloc_offset..(alloc_offset + bytes.len())].copy_from_slice(bytes);

        alloc_offset + padded_bytes_len
    }

    // Computes the padded size for a given size, e.g.:
    // padded32_size(20) == 32
    // padded32_size(32) == 32
    // padded32_size(40) == 64
    fn padded32_size(size: usize) -> usize {
        let r = size % 32;

        if r == 0 {
            size
        } else {
            size + 32 - r
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use pretty_assertions::assert_eq;
    use rand::Rng;

    #[test]
    fn decode_uint() {
        let uint: U256 = U256::exp10(18) + 1;

        let mut bs = [0u8; 32];
        uint.to_big_endian(&mut bs[..]);

        let v = Value::decode_from_slice(&bs, &vec![Type::Uint(256)]);

        assert_eq!(v, Ok(vec![Value::Uint(uint, 256)]));
    }

    #[test]
    fn decode_int() {
        let uint: U256 = U256::exp10(18) + 1;

        let mut bs = [0u8; 32];
        uint.to_big_endian(&mut bs[..]);

        let v = Value::decode_from_slice(&bs, &vec![Type::Int(256)]);

        assert_eq!(v, Ok(vec![Value::Int(uint, 256)]));
    }

    #[test]
    fn decode_address() {
        let addr = H160::random();

        let mut bs = [0u8; 32];
        &bs[12..32].copy_from_slice(addr.as_bytes());

        let v = Value::decode_from_slice(&bs, &vec![Type::Address]);

        assert_eq!(v, Ok(vec![Value::Address(addr)]));
    }

    #[test]
    fn decode_bool() {
        let mut bs = [0u8; 32];
        bs[31] = 1;

        let v = Value::decode_from_slice(&bs, &vec![Type::Bool]);

        assert_eq!(v, Ok(vec![Value::Bool(true)]));
    }

    #[test]
    fn decode_fixed_bytes() {
        let mut bs = [0u8; 32];
        for i in 1..16 {
            bs[i] = i as u8;
        }

        let v = Value::decode_from_slice(&bs, &vec![Type::FixedBytes(16)]);

        assert_eq!(v, Ok(vec![Value::FixedBytes(bs[0..16].to_vec())]));
    }

    #[test]
    fn decode_fixed_array() {
        let mut bs = [0u8; 128];

        // encode some data
        let uint1 = U256::from(5);
        let uint2 = U256::from(6);
        let uint3 = U256::from(7);
        let uint4 = U256::from(8);

        uint1.to_big_endian(&mut bs[0..32]);
        uint2.to_big_endian(&mut bs[32..64]);
        uint3.to_big_endian(&mut bs[64..96]);
        uint4.to_big_endian(&mut bs[96..128]);

        let uint_arr2 = Type::FixedArray(Box::new(Type::Uint(256)), 2);

        let v =
            Value::decode_from_slice(&bs, &vec![Type::FixedArray(Box::new(uint_arr2.clone()), 2)]);

        assert_eq!(
            v,
            Ok(vec![Value::FixedArray(
                vec![
                    Value::FixedArray(
                        vec![Value::Uint(uint1, 256), Value::Uint(uint2, 256)],
                        Type::Uint(256)
                    ),
                    Value::FixedArray(
                        vec![Value::Uint(uint3, 256), Value::Uint(uint4, 256)],
                        Type::Uint(256)
                    )
                ],
                uint_arr2
            )])
        );
    }

    #[test]
    fn decode_string() {
        let mut rng = rand::thread_rng();

        let mut bs = [0u8; 128];

        bs[31] = 0x20; // big-endian string offset

        let str_len: usize = rng.gen_range(0, 64);
        bs[63] = str_len as u8; // big-endian string size

        let chars = "abcdef0123456789".as_bytes();

        for i in 0..(str_len as usize) {
            bs[64 + i] = chars[rng.gen_range(0, chars.len())];
        }

        let v = Value::decode_from_slice(&bs, &vec![Type::String]);

        let expected_str = String::from_utf8(bs[64..(64 + str_len)].to_vec()).unwrap();
        assert_eq!(v, Ok(vec![Value::String(expected_str)]));
    }

    #[test]
    fn decode_bytes() {
        let mut rng = rand::thread_rng();

        let mut bs = [0u8; 128];
        bs[31] = 0x20; // big-endian bytes offset

        let bytes_len: usize = rng.gen_range(0, 64);
        bs[63] = bytes_len as u8; // big-endian bytes length

        for i in 0..(bytes_len as usize) {
            bs[64 + i] = rng.gen();
        }

        let v = Value::decode_from_slice(&bs, &vec![Type::Bytes]);

        assert_eq!(v, Ok(vec![Value::Bytes(bs[64..(64 + bytes_len)].to_vec())]));
    }

    #[test]
    fn decode_array() {
        let mut bs = [0u8; 192];
        bs[31] = 0x20; // big-endian array offset
        bs[63] = 2; // big-endian array length

        // encode some data
        let uint1 = U256::from(5);
        let uint2 = U256::from(6);
        let uint3 = U256::from(7);
        let uint4 = U256::from(8);

        uint1.to_big_endian(&mut bs[64..96]);
        uint2.to_big_endian(&mut bs[96..128]);
        uint3.to_big_endian(&mut bs[128..160]);
        uint4.to_big_endian(&mut bs[160..192]);

        let uint_arr2 = Type::FixedArray(Box::new(Type::Uint(256)), 2);

        let v = Value::decode_from_slice(&bs, &vec![Type::Array(Box::new(uint_arr2.clone()))]);

        assert_eq!(
            v,
            Ok(vec![Value::Array(
                vec![
                    Value::FixedArray(
                        vec![Value::Uint(uint1, 256), Value::Uint(uint2, 256)],
                        Type::Uint(256)
                    ),
                    Value::FixedArray(
                        vec![Value::Uint(uint3, 256), Value::Uint(uint4, 256)],
                        Type::Uint(256)
                    )
                ],
                uint_arr2
            )])
        );
    }

    #[test]
    fn decode_many() {
        // function f(string memory x, uint32 y, uint32[][2] memory z)
        let tys = vec![
            Type::String,
            Type::Uint(32),
            Type::FixedArray(Box::new(Type::Array(Box::new(Type::Uint(32)))), 2),
        ];

        // f("abc", 5, [[1, 2], [3]])
        let input = "0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000500000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000036162630000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000003";
        let mut bs = [0u8; 384];
        hex::decode_to_slice(input, &mut bs).unwrap();

        let v = Value::decode_from_slice(&bs, &tys);
        println!("{:?}", v);

        assert_eq!(
            v,
            Ok(vec![
                Value::String("abc".to_string()),
                Value::Uint(U256::from(5), 32),
                Value::FixedArray(
                    vec![
                        Value::Array(
                            vec![
                                Value::Uint(U256::from(1), 32),
                                Value::Uint(U256::from(2), 32),
                            ],
                            Type::Uint(32)
                        ),
                        Value::Array(vec![Value::Uint(U256::from(3), 32)], Type::Uint(32)),
                    ],
                    Type::Array(Box::new(Type::Uint(32)))
                ),
            ]),
        );
    }

    #[test]
    fn encode_uint() {
        let value = Value::Uint(U256::from(0xefcdab), 56);

        let mut expected_bytes = [0u8; 32].to_vec();
        expected_bytes[31] = 0xab;
        expected_bytes[30] = 0xcd;
        expected_bytes[29] = 0xef;

        assert_eq!(Value::encode(&vec![value]), expected_bytes);
    }

    #[test]
    fn encode_int() {
        let value = Value::Int(U256::from(0xabcdef), 56);

        let mut expected_bytes = [0u8; 32].to_vec();
        expected_bytes[31] = 0xef;
        expected_bytes[30] = 0xcd;
        expected_bytes[29] = 0xab;

        assert_eq!(Value::encode(&vec![value]), expected_bytes);
    }

    #[test]
    fn encode_address() {
        let addr = H160::random();
        let value = Value::Address(addr);

        let mut expected_bytes = [0u8; 32].to_vec();
        expected_bytes[12..32].copy_from_slice(addr.as_fixed_bytes());

        assert_eq!(Value::encode(&vec![value]), expected_bytes);
    }

    #[test]
    fn encode_bool() {
        let mut true_vec = [0u8; 32].to_vec();
        true_vec[31] = 1;

        let false_vec = [0u8; 32].to_vec();

        assert_eq!(Value::encode(&vec![Value::Bool(true)]), true_vec);
        assert_eq!(Value::encode(&vec![Value::Bool(false)]), false_vec);
    }

    #[test]
    fn encode_fixed_bytes() {
        let mut bytes = [0u8; 32].to_vec();
        for i in 0..16 {
            bytes[i] = i as u8;
        }

        assert_eq!(
            Value::encode(&vec![Value::FixedBytes(bytes[0..16].to_vec())]),
            bytes
        );
    }

    #[test]
    fn encode_fixed_array() {
        let uint1 = U256::from(57);
        let uint2 = U256::from(109);

        let value = Value::FixedArray(
            vec![Value::Uint(uint1, 56), Value::Uint(uint2, 56)],
            Type::Uint(56),
        );

        let mut expected_bytes = [0u8; 64];
        uint1.to_big_endian(&mut expected_bytes[0..32]);
        uint2.to_big_endian(&mut expected_bytes[32..64]);

        assert_eq!(Value::encode(&vec![value]), expected_bytes);
    }

    #[test]
    fn encode_string_and_bytes() {
        // Bytes and strings are encoded in the same way.

        let mut s = String::with_capacity(2890);
        s.reserve(2890);
        for i in 0..1000 {
            s += i.to_string().as_ref();
        }

        let mut expected_bytes = [0u8; 2976];
        expected_bytes[31] = 0x20; // big-endian offset
        expected_bytes[63] = 0x4a; // big-endian string size (2890 = 0xb4a)
        expected_bytes[62] = 0x0b;
        expected_bytes[64..(64 + 2890)].copy_from_slice(s.as_bytes());

        assert_eq!(Value::encode(&vec![Value::String(s)]), expected_bytes);
    }

    #[test]
    fn encode_array() {
        let addr1 = H160::random();
        let addr2 = H160::random();

        let value = Value::Array(
            vec![Value::Address(addr1), Value::Address(addr2)],
            Type::Address,
        );

        let mut expected_bytes = [0u8; 128];
        expected_bytes[31] = 0x20; // big-endian offset
        expected_bytes[63] = 2; // big-endian array length
        expected_bytes[76..96].copy_from_slice(addr1.as_fixed_bytes());
        expected_bytes[108..128].copy_from_slice(addr2.as_fixed_bytes());

        assert_eq!(Value::encode(&vec![value]), expected_bytes);
    }

    #[test]
    fn encode_many() {
        let values = vec![
            Value::String("abc".to_string()),
            Value::Uint(U256::from(5), 32),
            Value::FixedArray(
                vec![
                    Value::Array(
                        vec![
                            Value::Uint(U256::from(1), 32),
                            Value::Uint(U256::from(2), 32),
                        ],
                        Type::Uint(32),
                    ),
                    Value::Array(vec![Value::Uint(U256::from(3), 32)], Type::Uint(32)),
                ],
                Type::Array(Box::new(Type::Uint(32))),
            ),
        ];

        let expected = "0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000500000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000036162630000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000a000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000003";
        let encoded = hex::encode(Value::encode(&values));

        assert_eq!(encoded, expected);
    }
}