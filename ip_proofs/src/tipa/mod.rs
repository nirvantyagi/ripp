use algebra::{
    bytes::ToBytes,
    curves::{PairingEngine, ProjectiveCurve},
    fields::Field,
    groups::Group,
    to_bytes, UniformRand,
};
use digest::Digest;
use ff_fft::polynomial::DensePolynomial;
use itertools::Itertools;
use num_traits::identities::{One, Zero};
use rand::Rng;
use std::{marker::PhantomData, ops::MulAssign};

use crate::{
    gipa::{GIPAProof, GIPA},
    Error,
};
use dh_commitments::{
    afgho16::{AFGHOCommitmentG1, AFGHOCommitmentG2},
    pedersen::PedersenCommitment,
    DoublyHomomorphicCommitment,
};
use inner_products::{InnerProduct, MultiexponentiationInnerProduct};

pub mod structured_scalar_message;

//TODO: Could generalize: Don't need TIPA over G1 and G2, would work with G1 and G1 or over different pairing engines
pub trait TIPACompatibleSetup {}

impl<G: Group> TIPACompatibleSetup for PedersenCommitment<G> {}
impl<P: PairingEngine> TIPACompatibleSetup for AFGHOCommitmentG1<P> {}
impl<P: PairingEngine> TIPACompatibleSetup for AFGHOCommitmentG2<P> {}

//TODO: May need to add "reverse" MultiexponentiationInnerProduct to allow for MIP with G2 messages (because TIP hard-coded G1 left and G2 right)
pub struct TIPA<IP, LMC, RMC, IPC, P, D> {
    _inner_product: PhantomData<IP>,
    _left_commitment: PhantomData<LMC>,
    _right_commitment: PhantomData<RMC>,
    _inner_product_commitment: PhantomData<IPC>,
    _pair: PhantomData<P>,
    _digest: PhantomData<D>,
}

pub struct TIPAProof<IP, LMC, RMC, IPC, P, D>
where
    D: Digest,
    P: PairingEngine,
    IP: InnerProduct<
        LeftMessage = LMC::Message,
        RightMessage = RMC::Message,
        Output = IPC::Message,
    >,
    LMC: DoublyHomomorphicCommitment + TIPACompatibleSetup,
    RMC: DoublyHomomorphicCommitment<Scalar = LMC::Scalar> + TIPACompatibleSetup,
    IPC: DoublyHomomorphicCommitment<Scalar = LMC::Scalar>,
    RMC::Message: MulAssign<LMC::Scalar>,
    IPC::Message: MulAssign<LMC::Scalar>,
    RMC::Key: MulAssign<LMC::Scalar>,
    IPC::Key: MulAssign<LMC::Scalar>,
    RMC::Output: MulAssign<LMC::Scalar>,
    IPC::Output: MulAssign<LMC::Scalar>,
{
    gipa_proof: GIPAProof<IP, LMC, RMC, IPC, D>,
    final_ck: (LMC::Key, RMC::Key),
    final_ck_proof: (P::G2Projective, P::G1Projective),
    _pair: PhantomData<P>,
}

pub struct SRS<P: PairingEngine> {
    g_alpha_powers: Vec<P::G1Projective>,
    h_beta_powers: Vec<P::G2Projective>,
    g_beta: P::G1Projective,
    h_alpha: P::G2Projective,
}

pub struct VerifierSRS<P: PairingEngine> {
    g: P::G1Projective,
    h: P::G2Projective,
    g_beta: P::G1Projective,
    h_alpha: P::G2Projective,
}

impl<P: PairingEngine> SRS<P> {
    pub fn get_commitment_keys(&self) -> (Vec<P::G2Projective>, Vec<P::G1Projective>) {
        let ck_1 = self.h_beta_powers.iter().step_by(2).cloned().collect();
        let ck_2 = self.g_alpha_powers.iter().step_by(2).cloned().collect();
        (ck_1, ck_2)
    }

    pub fn get_verifier_key(&self) -> VerifierSRS<P> {
        VerifierSRS {
            g: self.g_alpha_powers[0].clone(),
            h: self.h_beta_powers[0].clone(),
            g_beta: self.g_beta.clone(),
            h_alpha: self.h_alpha.clone(),
        }
    }
}

impl<IP, LMC, RMC, IPC, P, D> TIPA<IP, LMC, RMC, IPC, P, D>
where
    D: Digest,
    P: PairingEngine,
    IP: InnerProduct<
        LeftMessage = LMC::Message,
        RightMessage = RMC::Message,
        Output = IPC::Message,
    >,
    LMC: DoublyHomomorphicCommitment<Scalar = P::Fr, Key = P::G2Projective> + TIPACompatibleSetup,
    RMC: DoublyHomomorphicCommitment<Scalar = LMC::Scalar, Key = P::G1Projective>
        + TIPACompatibleSetup,
    IPC: DoublyHomomorphicCommitment<Scalar = LMC::Scalar>,
    LMC::Message: MulAssign<P::Fr>,
    RMC::Message: MulAssign<P::Fr>,
    IPC::Message: MulAssign<P::Fr>,
    IPC::Key: MulAssign<P::Fr>,
    LMC::Output: MulAssign<P::Fr>,
    RMC::Output: MulAssign<P::Fr>,
    IPC::Output: MulAssign<P::Fr>,
{
    pub fn setup<R: Rng>(rng: &mut R, size: usize) -> Result<(SRS<P>, IPC::Key), Error> {
        let alpha = <P::Fr>::rand(rng);
        let beta = <P::Fr>::rand(rng);
        let g = <P::G1Projective>::prime_subgroup_generator();
        let h = <P::G2Projective>::prime_subgroup_generator();
        Ok((
            SRS {
                g_alpha_powers: structured_generators_scalar_power(2 * size - 1, &g, &alpha),
                h_beta_powers: structured_generators_scalar_power(2 * size - 1, &h, &beta),
                g_beta: <P::G1Projective as Group>::mul(&g, &beta),
                h_alpha: <P::G2Projective as Group>::mul(&h, &alpha),
            },
            IPC::setup(rng, 1)?.pop().unwrap(),
        ))
    }
    pub fn prove(
        srs: &SRS<P>,
        values: (&[IP::LeftMessage], &[IP::RightMessage]),
        ck: (&[LMC::Key], &[RMC::Key], &IPC::Key),
    ) -> Result<TIPAProof<IP, LMC, RMC, IPC, P, D>, Error> {
        Self::prove_with_srs_shift(srs, values, ck, &<P::Fr>::one())
    }

    // Shifts KZG proof for left message by scalar r (used for efficient composition with aggregation protocols)
    // LMC commitment key should already be shifted before being passed as input
    pub fn prove_with_srs_shift(
        srs: &SRS<P>,
        values: (&[IP::LeftMessage], &[IP::RightMessage]),
        ck: (&[LMC::Key], &[RMC::Key], &IPC::Key),
        r_shift: &P::Fr,
    ) -> Result<TIPAProof<IP, LMC, RMC, IPC, P, D>, Error> {
        // Run GIPA
        let (proof, aux) = <GIPA<IP, LMC, RMC, IPC, D>>::prove_with_aux(
            values,
            (ck.0, ck.1, &vec![ck.2.clone()]),
        )?;

        // Prove final commitment keys are wellformed
        let (ck_a_final, ck_b_final) = aux.ck_base;
        let transcript = aux.r_transcript;
        let transcript_inverse = transcript.iter().map(|x| x.inverse().unwrap()).collect();
        let r_inverse = r_shift.inverse().unwrap();

        let ck_b_polynomial = DensePolynomial::from_coefficients_slice(
            &polynomial_coefficients_from_transcript(&transcript, &<P::Fr>::one()),
        );
        let ck_a_polynomial = DensePolynomial::from_coefficients_slice(
            &polynomial_coefficients_from_transcript(&transcript_inverse, &r_inverse),
        );
        assert_eq!(srs.g_alpha_powers.len(), ck_a_polynomial.coeffs.len());

        // KZG challenge point
        let mut counter_nonce: usize = 0;
        let c = loop {
            let mut hash_input = Vec::new();
            hash_input.extend_from_slice(&counter_nonce.to_be_bytes()[..]);
            //TODO: Should use CanonicalSerialize instead of ToBytes
            hash_input.extend_from_slice(&to_bytes![
                transcript.first().unwrap(),
                ck_a_final,
                ck_b_final
            ]?);
            if let Some(c) = LMC::Scalar::from_random_bytes(&D::digest(&hash_input)) {
                break c;
            };
            counter_nonce += 1;
        };

        // Complete KZG proofs
        let ck_a_polynomial_c_eval =
            polynomial_evaluation_product_form_from_transcript(&transcript_inverse, &c, &r_inverse);
        let ck_b_polynomial_c_eval =
            polynomial_evaluation_product_form_from_transcript(&transcript, &c, &<P::Fr>::one());

        let quotient_polynomial_a = &(&ck_a_polynomial
            - &DensePolynomial::from_coefficients_vec(vec![ck_a_polynomial_c_eval]))
            / &(DensePolynomial::from_coefficients_vec(vec![-c, LMC::Scalar::one()]));
        let quotient_polynomial_b = &(&ck_b_polynomial
            - &DensePolynomial::from_coefficients_vec(vec![ck_b_polynomial_c_eval]))
            / &(DensePolynomial::from_coefficients_vec(vec![-c, LMC::Scalar::one()]));

        let mut quotient_polynomial_a_coeffs = quotient_polynomial_a.coeffs;
        quotient_polynomial_a_coeffs.resize(srs.g_alpha_powers.len(), <P::Fr>::zero());
        let mut quotient_polynomial_b_coeffs = quotient_polynomial_b.coeffs;
        quotient_polynomial_b_coeffs.resize(srs.g_alpha_powers.len(), <P::Fr>::zero());

        let quotient_a_proof = MultiexponentiationInnerProduct::inner_product(
            &srs.h_beta_powers,
            &quotient_polynomial_a_coeffs,
        )?;
        let quotient_b_proof = MultiexponentiationInnerProduct::inner_product(
            &srs.g_alpha_powers,
            &quotient_polynomial_b_coeffs,
        )?;

        Ok(TIPAProof {
            gipa_proof: proof,
            final_ck: (ck_a_final, ck_b_final),
            final_ck_proof: (quotient_a_proof, quotient_b_proof),
            _pair: PhantomData,
        })
    }

    pub fn verify(
        v_srs: &VerifierSRS<P>,
        ck_t: &IPC::Key,
        com: (&LMC::Output, &RMC::Output, &IPC::Output),
        proof: &TIPAProof<IP, LMC, RMC, IPC, P, D>,
    ) -> Result<bool, Error> {
        Self::verify_with_srs_shift(v_srs, ck_t, com, proof, &<P::Fr>::one())
    }

    pub fn verify_with_srs_shift(
        v_srs: &VerifierSRS<P>,
        ck_t: &IPC::Key,
        com: (&LMC::Output, &RMC::Output, &IPC::Output),
        proof: &TIPAProof<IP, LMC, RMC, IPC, P, D>,
        r_shift: &P::Fr,
    ) -> Result<bool, Error> {
        let (base_com, transcript) =
            GIPA::verify_recursive_challenge_transcript(com, &proof.gipa_proof)?;
        let transcript_inverse = transcript.iter().map(|x| x.inverse().unwrap()).collect();

        // Verify commitment keys wellformed
        let (ck_a_final, ck_b_final) = &proof.final_ck;
        let (ck_a_proof, ck_b_proof) = &proof.final_ck_proof;

        // KZG challenge point
        let mut counter_nonce: usize = 0;
        let c = loop {
            let mut hash_input = Vec::new();
            hash_input.extend_from_slice(&counter_nonce.to_be_bytes()[..]);
            //TODO: Should use CanonicalSerialize instead of ToBytes
            hash_input.extend_from_slice(&to_bytes![
                transcript.first().unwrap(),
                ck_a_final,
                ck_b_final
            ]?);
            if let Some(c) = LMC::Scalar::from_random_bytes(&D::digest(&hash_input)) {
                break c;
            };
            counter_nonce += 1;
        };

        let ck_a_polynomial_c_eval = polynomial_evaluation_product_form_from_transcript(
            &transcript_inverse,
            &c,
            &r_shift.inverse().unwrap(),
        );
        let ck_b_polynomial_c_eval =
            polynomial_evaluation_product_form_from_transcript(&transcript, &c, &<P::Fr>::one());

        let ck_a_valid =
            P::pairing(
                v_srs.g.clone(),
                ck_a_final.clone() - &v_srs.h.mul(ck_a_polynomial_c_eval),
            ) == P::pairing(v_srs.g_beta.clone() - &v_srs.g.mul(c), ck_a_proof.clone());
        let ck_b_valid =
            P::pairing(
                ck_b_final.clone() - &v_srs.g.mul(ck_b_polynomial_c_eval),
                v_srs.h.clone(),
            ) == P::pairing(ck_b_proof.clone(), v_srs.h_alpha.clone() - &v_srs.h.mul(c));

        // Verify base inner product commitment
        let (com_a, com_b, com_t) = base_com;
        let a_base = vec![proof.gipa_proof.r_base.0.clone()];
        let b_base = vec![proof.gipa_proof.r_base.1.clone()];
        let t_base = vec![IP::inner_product(&a_base, &b_base)?];
        let base_valid = LMC::verify(&vec![ck_a_final.clone()], &a_base, &com_a)?
            && RMC::verify(&vec![ck_b_final.clone()], &b_base, &com_b)?
            && IPC::verify(&vec![ck_t.clone()], &t_base, &com_t)?;

        Ok(ck_a_valid && ck_b_valid && base_valid)
    }
}

pub fn structured_generators_scalar_power<G: Group>(
    num: usize,
    g: &G,
    s: &G::ScalarField,
) -> Vec<G> {
    let mut generators = Vec::new();
    let mut pow_s = G::ScalarField::one();
    for _ in 0..num {
        generators.push(g.mul(&pow_s));
        pow_s *= s;
    }
    generators
}

fn polynomial_evaluation_product_form_from_transcript<F: Field>(
    transcript: &Vec<F>,
    z: &F,
    r_shift: &F,
) -> F {
    let mut power_2_zr = (z.clone() * z) * r_shift;
    let mut product_form = Vec::new();
    for x in transcript.iter() {
        product_form.push(F::one() + (x.clone() * &power_2_zr));
        power_2_zr *= power_2_zr;
    }
    product_form.iter().product()
}

fn polynomial_coefficients_from_transcript<F: Field>(transcript: &Vec<F>, r_shift: &F) -> Vec<F> {
    let mut coefficients = vec![F::one()];
    let mut power_2_r = r_shift.clone();
    for (i, x) in transcript.iter().enumerate() {
        for j in 0..(2_usize).pow(i as u32) {
            coefficients.push(coefficients[j] * &(x.clone() * &power_2_r));
        }
        power_2_r *= power_2_r;
    }
    // Interleave with 0 coefficients
    coefficients
        .iter()
        .interleave(vec![F::zero()].iter().cycle().take(coefficients.len() - 1))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use algebra::{bls12_381::Bls12_381, curves::PairingEngine, UniformRand};
    use blake2::Blake2b;
    use rand::{rngs::StdRng, SeedableRng};

    use crate::tipa::structured_scalar_message::structured_scalar_power;
    use dh_commitments::{
        afgho16::{AFGHOCommitmentG1, AFGHOCommitmentG2},
        identity::IdentityCommitment,
        pedersen::PedersenCommitment,
        random_generators,
    };
    use inner_products::{
        ExtensionFieldElement, InnerProduct, MultiexponentiationInnerProduct, PairingInnerProduct,
        ScalarInnerProduct,
    };

    type GC1 = AFGHOCommitmentG1<Bls12_381>;
    type GC2 = AFGHOCommitmentG2<Bls12_381>;
    type SC1 = PedersenCommitment<<Bls12_381 as PairingEngine>::G1Projective>;
    type SC2 = PedersenCommitment<<Bls12_381 as PairingEngine>::G2Projective>;

    const TEST_SIZE: usize = 8;

    #[test]
    fn pairing_inner_product_test() {
        type IP = PairingInnerProduct<Bls12_381>;
        type IPC =
            IdentityCommitment<ExtensionFieldElement<Bls12_381>, <Bls12_381 as PairingEngine>::Fr>;
        type PairingTIPA = TIPA<IP, GC1, GC2, IPC, Bls12_381, Blake2b>;

        let mut rng = StdRng::seed_from_u64(0u64);
        let (srs, ck_t) = PairingTIPA::setup(&mut rng, TEST_SIZE).unwrap();
        let (ck_a, ck_b) = srs.get_commitment_keys();
        let v_srs = srs.get_verifier_key();
        let m_a = random_generators(&mut rng, TEST_SIZE);
        let m_b = random_generators(&mut rng, TEST_SIZE);
        let com_a = GC1::commit(&ck_a, &m_a).unwrap();
        let com_b = GC2::commit(&ck_b, &m_b).unwrap();
        let t = vec![IP::inner_product(&m_a, &m_b).unwrap()];
        let com_t = IPC::commit(&vec![ck_t.clone()], &t).unwrap();

        let proof = PairingTIPA::prove(&srs, (&m_a, &m_b), (&ck_a, &ck_b, &ck_t)).unwrap();

        assert!(PairingTIPA::verify(&v_srs, &ck_t, (&com_a, &com_b, &com_t), &proof).unwrap());
    }

    #[test]
    fn multiexponentiation_inner_product_test() {
        type IP = MultiexponentiationInnerProduct<<Bls12_381 as PairingEngine>::G1Projective>;
        type IPC = IdentityCommitment<
            <Bls12_381 as PairingEngine>::G1Projective,
            <Bls12_381 as PairingEngine>::Fr,
        >;
        type MultiExpTIPA = TIPA<IP, GC1, SC1, IPC, Bls12_381, Blake2b>;

        let mut rng = StdRng::seed_from_u64(0u64);
        let (srs, ck_t) = MultiExpTIPA::setup(&mut rng, TEST_SIZE).unwrap();
        let (ck_a, ck_b) = srs.get_commitment_keys();
        let v_srs = srs.get_verifier_key();
        let m_a = random_generators(&mut rng, TEST_SIZE);
        let mut m_b = Vec::new();
        for _ in 0..TEST_SIZE {
            m_b.push(<Bls12_381 as PairingEngine>::Fr::rand(&mut rng));
        }
        let com_a = GC1::commit(&ck_a, &m_a).unwrap();
        let com_b = SC1::commit(&ck_b, &m_b).unwrap();
        let t = vec![IP::inner_product(&m_a, &m_b).unwrap()];
        let com_t = IPC::commit(&vec![ck_t.clone()], &t).unwrap();

        let proof = MultiExpTIPA::prove(&srs, (&m_a, &m_b), (&ck_a, &ck_b, &ck_t)).unwrap();

        assert!(MultiExpTIPA::verify(&v_srs, &ck_t, (&com_a, &com_b, &com_t), &proof).unwrap());
    }

    #[test]
    fn scalar_inner_product_test() {
        type IP = ScalarInnerProduct<<Bls12_381 as PairingEngine>::Fr>;
        type IPC =
            IdentityCommitment<<Bls12_381 as PairingEngine>::Fr, <Bls12_381 as PairingEngine>::Fr>;
        type ScalarTIPA = TIPA<IP, SC2, SC1, IPC, Bls12_381, Blake2b>;

        let mut rng = StdRng::seed_from_u64(0u64);
        let (srs, ck_t) = ScalarTIPA::setup(&mut rng, TEST_SIZE).unwrap();
        let (ck_a, ck_b) = srs.get_commitment_keys();
        let v_srs = srs.get_verifier_key();
        let mut m_a = Vec::new();
        let mut m_b = Vec::new();
        for _ in 0..TEST_SIZE {
            m_a.push(<Bls12_381 as PairingEngine>::Fr::rand(&mut rng));
            m_b.push(<Bls12_381 as PairingEngine>::Fr::rand(&mut rng));
        }
        let com_a = SC2::commit(&ck_a, &m_a).unwrap();
        let com_b = SC1::commit(&ck_b, &m_b).unwrap();
        let t = vec![IP::inner_product(&m_a, &m_b).unwrap()];
        let com_t = IPC::commit(&vec![ck_t.clone()], &t).unwrap();

        let proof = ScalarTIPA::prove(&srs, (&m_a, &m_b), (&ck_a, &ck_b, &ck_t)).unwrap();

        assert!(ScalarTIPA::verify(&v_srs, &ck_t, (&com_a, &com_b, &com_t), &proof).unwrap());
    }

    #[test]
    fn pairing_inner_product_with_srs_shift_test() {
        type IP = PairingInnerProduct<Bls12_381>;
        type IPC =
            IdentityCommitment<ExtensionFieldElement<Bls12_381>, <Bls12_381 as PairingEngine>::Fr>;
        type PairingTIPA = TIPA<IP, GC1, GC2, IPC, Bls12_381, Blake2b>;

        let mut rng = StdRng::seed_from_u64(0u64);
        let (srs, ck_t) = PairingTIPA::setup(&mut rng, TEST_SIZE).unwrap();
        let (ck_a, ck_b) = srs.get_commitment_keys();
        let v_srs = srs.get_verifier_key();

        let m_a = random_generators(&mut rng, TEST_SIZE);
        let m_b = random_generators(&mut rng, TEST_SIZE);
        let com_a = GC1::commit(&ck_a, &m_a).unwrap();
        let com_b = GC2::commit(&ck_b, &m_b).unwrap();

        let r_scalar = <<Bls12_381 as PairingEngine>::Fr>::rand(&mut rng);
        let r_vec = structured_scalar_power(TEST_SIZE, &r_scalar);
        let m_a_r = m_a
            .iter()
            .zip(&r_vec)
            .map(|(a, r)| a.mul(r))
            .collect::<Vec<<Bls12_381 as PairingEngine>::G1Projective>>();
        let ck_a_r = ck_a
            .iter()
            .zip(&r_vec)
            .map(|(ck, r)| ck.mul(&r.inverse().unwrap()))
            .collect::<Vec<<Bls12_381 as PairingEngine>::G2Projective>>();

        let t = vec![IP::inner_product(&m_a_r, &m_b).unwrap()];
        let com_t = IPC::commit(&vec![ck_t.clone()], &t).unwrap();

        assert_eq!(com_a, IP::inner_product(&m_a_r, &ck_a_r).unwrap());

        let proof = PairingTIPA::prove_with_srs_shift(
            &srs,
            (&m_a_r, &m_b),
            (&ck_a_r, &ck_b, &ck_t),
            &r_scalar,
        )
        .unwrap();

        assert!(PairingTIPA::verify_with_srs_shift(
            &v_srs,
            &ck_t,
            (&com_a, &com_b, &com_t),
            &proof,
            &r_scalar
        )
        .unwrap());
    }
}
