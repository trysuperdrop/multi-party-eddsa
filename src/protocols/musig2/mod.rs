#![allow(non_snake_case)]

//! Simple ed25519
//!
//! See https://tools.ietf.org/html/rfc8032
//! This is an implementation of the Musig2 protocol as shown in https://eprint.iacr.org/2020/1261.pdf with the addition named Musig2* suggested in Section B of the paper.
//! We implement the v = 2 (NUMBER_OF_NONCES) version, meaning there are 2 nonces generated by each party.

use super::{ExpandedKeyPair, Signature};
use curv::cryptographic_primitives::hashing::DigestExt;
use curv::elliptic::curves::{Ed25519, Point, Scalar};
use rand::Rng;
use sha2::{digest::Digest, Sha512};

const NUMBER_OF_NONCES: usize = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PublicKeyAgg {
    pub agg_public_key: Point<Ed25519>,
    pub musig_coefficient: Scalar<Ed25519>,
}

impl PublicKeyAgg {
    pub fn key_aggregation_n(
        mut public_keys: Vec<Point<Ed25519>>,
        my_public_key: &Point<Ed25519>,
    ) -> PublicKeyAgg {
        let mut my_coeff = Scalar::zero();
        let mut sum = Point::zero();
        public_keys.sort_by(|left, right| left.to_bytes(false).cmp(&right.to_bytes(false)));
        let mut second_public_key = &public_keys[0];
        for public_key in &public_keys[1..] {
            if public_key
                .to_bytes(false)
                .gt(&public_keys[0].to_bytes(false))
            {
                second_public_key = public_key;
                break;
            }
        }

        public_keys
            .iter()
            .for_each(|public_key| {
                let mut musig_coefficient: Scalar<Ed25519> = Scalar::from(1);
                if public_key != second_public_key {
                    let mut hasher = Sha512::new().chain(&[1]).chain(&*public_key.to_bytes(true));
                    for pk in &public_keys {
                        hasher.update(&*pk.to_bytes(true));
                    }
                    musig_coefficient = hasher.result_scalar();
                }

                let a_i = public_key * &musig_coefficient;
                if public_key == my_public_key {
                    my_coeff = musig_coefficient;
                }
                sum = &sum + a_i
            });
        PublicKeyAgg {
            agg_public_key: sum,
            musig_coefficient: my_coeff,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PartialNonces {
    pub r: [Scalar<Ed25519>; NUMBER_OF_NONCES],
    pub R: [Point<Ed25519>; NUMBER_OF_NONCES],
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PartialSignature {
    pub R: Point<Ed25519>,
    pub my_partial_s: Scalar<Ed25519>,
}

pub fn generate_partial_nonces(
    keys: &ExpandedKeyPair,
    message: Option<&[u8]>,
    rng: &mut impl Rng,
) -> PartialNonces {
    // here we deviate from the spec, by introducing  non-deterministic element (random number)
    // to the nonce, this is important for MPC implementations
    let r: [Scalar<Ed25519>; NUMBER_OF_NONCES] = [(); NUMBER_OF_NONCES].map(|_| {
        Sha512::new()
            .chain(&[2])
            .chain(&*keys.expanded_private_key.prefix.to_bytes())
            .chain(message.unwrap_or(&[]))
            .chain(rng.gen::<[u8; 32]>())
            .result_scalar()
    });
    let R: [Point<Ed25519>; NUMBER_OF_NONCES] =
        r.clone().map(|scalar| Point::generator() * &scalar);
    PartialNonces { r, R }
}

pub fn partial_sign(
    nonces_from_other_parties: &[[Point<Ed25519>; NUMBER_OF_NONCES]],
    my_partial_nonces: PartialNonces,
    agg_public_key: &PublicKeyAgg,
    my_keypair: &ExpandedKeyPair,
    message: &[u8],
) -> PartialSignature {
    // Sum up partial nonces from all parties
    let R: [Point<Ed25519>; NUMBER_OF_NONCES] = nonces_from_other_parties.iter().fold(
        my_partial_nonces.R,
        |mut accumulator: [Point<Ed25519>; NUMBER_OF_NONCES],
         partial_nonce_array: &[Point<Ed25519>; NUMBER_OF_NONCES]| {
            for (accum_nonce, nonce) in accumulator.iter_mut().zip(partial_nonce_array) {
                *accum_nonce = &*accum_nonce + nonce;
            }
            accumulator
        }
    );

    // Compute b as hash of nonces
    let mut hasher = Sha512::new()
        .chain(&[3])
        .chain(&*agg_public_key.agg_public_key.to_bytes(false));
    for nonce in &R {
        hasher.update(&*nonce.to_bytes(false));
    }
    hasher.update(message);
    let b: Scalar<Ed25519> = hasher.result_scalar();
    
    // Compute effective nonce
    let (effective_R, effective_r, _) = R[1..]
        .iter()
        .zip(my_partial_nonces.r[1..].iter())
        .fold(
            (R[0].clone(), my_partial_nonces.r[0].clone(), b.clone()),
            |accumulator: (Point<Ed25519>, Scalar<Ed25519>, Scalar<Ed25519>),
             nonce_tuple: (&Point<Ed25519>, &Scalar<Ed25519>)| {
                (
                    accumulator.0 + &accumulator.2 * nonce_tuple.0,
                    &accumulator.1 + accumulator.2 * nonce_tuple.1,
                    accumulator.1 * &b,
                )
            },
        );
    
    // Compute Fiat-Shamir challenge of signature
    let sig_challenge = Signature::k(&effective_R, &agg_public_key.agg_public_key, message);

    // Computes the partial signature
    let partial_signature: Scalar<Ed25519> = sig_challenge
        * &agg_public_key.musig_coefficient
        * &my_keypair.expanded_private_key.private_key
        + effective_r;
    
    PartialSignature {
        R: effective_R,
        my_partial_s: partial_signature,
    }
}

pub fn aggregate_partial_signatures(
    my_partial_sig: &PartialSignature,
    partial_sigs_from_other_parties: &[Scalar<Ed25519>],
) -> Signature {
    let aggregate_signature = partial_sigs_from_other_parties
        .iter()
        .sum::<Scalar<Ed25519>>()
        + &my_partial_sig.my_partial_s;

    Signature {
        R: my_partial_sig.R.clone(),
        s: aggregate_signature,
    }
}

mod test;