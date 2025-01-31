use std::{fs::File, io::Read, str::FromStr};

use bitcoin_hashes::{hash160, Hash};
use secp256k1::{Message, Parity::Even, PublicKey, Scalar, SecretKey, XOnlyPublicKey};
use serde_json::from_str;

use super::structs::{OutputWithSignature, TestData};

use silentpayments::Error;
use std::convert::TryInto;
use std::io::{self, Cursor};

fn deser_compact_size(f: &mut Cursor<&Vec<u8>>) -> io::Result<u64> {
    let mut buf = [0; 8];
    f.read_exact(&mut buf[..1])?;
    match buf[0] {
        0xfd => {
            f.read_exact(&mut buf[..2])?;
            Ok(u16::from_le_bytes(buf[..2].try_into().unwrap()) as u64)
        }
        0xfe => {
            f.read_exact(&mut buf[..4])?;
            Ok(u32::from_le_bytes(buf[..4].try_into().unwrap()) as u64)
        }
        0xff => {
            f.read_exact(&mut buf)?;
            Ok(u64::from_le_bytes(buf))
        }
        _ => Ok(buf[0] as u64),
    }
}

fn deser_string(f: &mut Cursor<&Vec<u8>>) -> io::Result<Vec<u8>> {
    let size = deser_compact_size(f)? as usize;
    let mut buf = vec![0; size];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn deser_string_vector(f: &mut Cursor<&Vec<u8>>) -> io::Result<Vec<Vec<u8>>> {
    // Check if the buffer is empty before attempting to deserialize the size
    if f.get_ref().is_empty() {
        return Ok(Vec::new()); // Return an empty vector if the buffer is empty
    }
    let size = deser_compact_size(f)? as usize;
    let mut vec = Vec::with_capacity(size);
    for _ in 0..size {
        vec.push(deser_string(f)?);
    }
    Ok(vec)
}

// ** Putting all the pubkey extraction logic in the test utils for now. **
// NUMS_H (defined in BIP340)
const NUMS_H: [u8; 32] = [
    0x50, 0x92, 0x9b, 0x74, 0xc1, 0xa0, 0x49, 0x54, 0xb7, 0x8b, 0x4b, 0x60, 0x35, 0xe9, 0x7a, 0x5e,
    0x07, 0x8a, 0x5a, 0x0f, 0x28, 0xec, 0x96, 0xd5, 0x47, 0xbf, 0xee, 0x9a, 0xce, 0x80, 0x3a, 0xc0,
];

// Define OP_CODES used in script template matching for readability
const OP_1: u8 = 0x51;
const OP_0: u8 = 0x00;
const OP_PUSHBYTES_20: u8 = 0x14;
const OP_PUSHBYTES_32: u8 = 0x20;
const OP_HASH160: u8 = 0xA9;
const OP_EQUAL: u8 = 0x87;
const OP_DUP: u8 = 0x76;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_CHECKSIG: u8 = 0xAC;

// Only compressed pubkeys are supported for silent payments
const COMPRESSED_PUBKEY_SIZE: usize = 33;

pub struct VinData {
    pub script_sig: Vec<u8>,
    pub txinwitness: Vec<Vec<u8>>,
    pub script_pub_key: Vec<u8>,
}

// script templates for inputs allowed in BIP352 shared secret derivation
pub fn is_p2tr(spk: &[u8]) -> bool {
    matches!(spk, [OP_1, OP_PUSHBYTES_32, ..] if spk.len() == 34)
}

fn is_p2wpkh(spk: &[u8]) -> bool {
    matches!(spk, [OP_0, OP_PUSHBYTES_20, ..] if spk.len() == 22)
}

fn is_p2sh(spk: &[u8]) -> bool {
    matches!(spk, [OP_HASH160, OP_PUSHBYTES_20, .., OP_EQUAL] if spk.len() == 23)
}

fn is_p2pkh(spk: &[u8]) -> bool {
    matches!(spk, [OP_DUP, OP_HASH160, OP_PUSHBYTES_20, .., OP_EQUALVERIFY, OP_CHECKSIG] if spk.len() == 25)
}

pub fn get_pubkey_from_input(vin: &VinData) -> Result<Option<PublicKey>, Error> {
    if is_p2pkh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (true, false) => {
                let spk_hash = &vin.script_pub_key[3..23];
                for i in (COMPRESSED_PUBKEY_SIZE..=vin.script_sig.len()).rev() {
                    if let Some(pubkey_bytes) = &vin.script_sig.get(i - COMPRESSED_PUBKEY_SIZE..i) {
                        let pubkey_hash = hash160::Hash::hash(pubkey_bytes);
                        if pubkey_hash.to_byte_array() == spk_hash {
                            return Ok(Some(PublicKey::from_slice(pubkey_bytes)?));
                        }
                    } else {
                        return Ok(None);
                    }
                }
            }
            (_, true) => {
                return Err(Error::InvalidVin(
                    "Empty script_sig for spending a p2pkh".to_owned(),
                ))
            }
            (false, _) => {
                return Err(Error::InvalidVin(
                    "non empty witness for spending a p2pkh".to_owned(),
                ))
            }
        }
    } else if is_p2sh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, false) => {
                let redeem_script = &vin.script_sig[1..];
                if is_p2wpkh(redeem_script) {
                    if let Some(value) = vin.txinwitness.last() {
                        match (
                            PublicKey::from_slice(value),
                            value.len() == COMPRESSED_PUBKEY_SIZE,
                        ) {
                            (Ok(pubkey), true) => {
                                return Ok(Some(pubkey));
                            }
                            (_, false) => {
                                return Ok(None);
                            }
                            // Not sure how we could get an error here, so just return none for now
                            // if the pubkey cant be parsed
                            (Err(_), _) => {
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            (_, true) => {
                return Err(Error::InvalidVin(
                    "Empty script_sig for spending a p2sh".to_owned(),
                ))
            }
            (true, false) => return Ok(None),
        }
    } else if is_p2wpkh(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, true) => {
                if let Some(value) = vin.txinwitness.last() {
                    match (
                        PublicKey::from_slice(value),
                        value.len() == COMPRESSED_PUBKEY_SIZE,
                    ) {
                        (Ok(pubkey), true) => {
                            return Ok(Some(pubkey));
                        }
                        (_, false) => {
                            return Ok(None);
                        }
                        // Not sure how we could get an error here, so just return none for now
                        // if the pubkey cant be parsed
                        (Err(_), _) => {
                            return Ok(None);
                        }
                    }
                } else {
                    return Err(Error::InvalidVin("Empty witness".to_owned()));
                }
            }
            (_, false) => {
                return Err(Error::InvalidVin(
                    "Non empty script sig for spending a segwit output".to_owned(),
                ))
            }
            (true, _) => {
                return Err(Error::InvalidVin(
                    "Empty witness for spending a segwit output".to_owned(),
                ))
            }
        }
    } else if is_p2tr(&vin.script_pub_key) {
        match (&vin.txinwitness.is_empty(), &vin.script_sig.is_empty()) {
            (false, true) => {
                // check for the optional annex
                let annex = match vin.txinwitness.last().and_then(|value| value.first()) {
                    Some(&0x50) => 1,
                    Some(_) => 0,
                    None => return Err(Error::InvalidVin("Empty or invalid witness".to_owned())),
                };

                // Check for script path
                let stack_size = vin.txinwitness.len();
                if stack_size > annex && vin.txinwitness[stack_size - annex - 1][1..33] == NUMS_H {
                    return Ok(None);
                }

                // Return the pubkey from the script pubkey
                return XOnlyPublicKey::from_slice(&vin.script_pub_key[2..34])
                    .map_err(Error::Secp256k1Error)
                    .map(|x_only_public_key| {
                        Some(PublicKey::from_x_only_public_key(x_only_public_key, Even))
                    });
            }
            (_, false) => {
                return Err(Error::InvalidVin(
                    "Non empty script sig for spending a segwit output".to_owned(),
                ))
            }
            (true, _) => {
                return Err(Error::InvalidVin(
                    "Empty witness for spending a segwit output".to_owned(),
                ))
            }
        }
    }
    Ok(None)
}

pub fn read_file() -> Vec<TestData> {
    let mut file = File::open("tests/resources/send_and_receive_test_vectors.json").unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    from_str(&contents).unwrap()
}

pub fn decode_outputs_to_check(outputs: &Vec<String>) -> Vec<XOnlyPublicKey> {
    outputs
        .iter()
        .map(|x| XOnlyPublicKey::from_str(x).unwrap())
        .collect()
}

pub fn decode_recipients(recipients: &Vec<(String, f32)>) -> Vec<String> {
    recipients
        .iter()
        .map(|(sp_addr_str, _)| sp_addr_str.to_owned())
        .collect()
}

pub fn verify_and_calculate_signatures(
    key_tweaks: Vec<Scalar>,
    b_spend: SecretKey,
) -> Result<Vec<OutputWithSignature>, secp256k1::Error> {
    let secp = secp256k1::Secp256k1::new();

    let msg = Message::from_digest(bitcoin_hashes::sha256::Hash::hash(b"message").to_byte_array());
    let aux = bitcoin_hashes::sha256::Hash::hash(b"random auxiliary data").to_byte_array();

    let mut res: Vec<OutputWithSignature> = vec![];
    for tweak in key_tweaks {
        // Add the tweak to the b_spend to get the final key
        let k = b_spend.add_tweak(&tweak)?;

        // get public key
        let P = k.x_only_public_key(&secp).0;

        // Sign the message with schnorr
        let sig = secp.sign_schnorr_with_aux_rand(&msg, &k.keypair(&secp), &aux);

        // Verify the message is correct
        secp.verify_schnorr(&sig, &msg, &P)?;

        // Push result to list
        res.push(OutputWithSignature {
            pub_key: P.to_string(),
            priv_key_tweak: hex::encode(tweak.to_be_bytes()),
            signature: sig.to_string(),
        });
    }
    Ok(res)
}

pub fn sender_get_a_sum_secret_keys(input: &Vec<(SecretKey, bool)>) -> SecretKey {
    let secp = secp256k1::Secp256k1::new();

    let mut negated_keys: Vec<SecretKey> = vec![];

    for (key, is_xonly) in input {
        let (_, parity) = key.x_only_public_key(&secp);

        if *is_xonly && parity == secp256k1::Parity::Odd {
            negated_keys.push(key.negate());
        } else {
            negated_keys.push(*key);
        }
    }

    let (head, tail) = negated_keys.split_first().unwrap();

    let result: SecretKey = tail
        .iter()
        .fold(*head, |acc, &item| acc.add_tweak(&item.into()).unwrap());

    result
}
