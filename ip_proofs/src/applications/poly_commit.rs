use algebra::{
    curves::{PairingEngine, ProjectiveCurve},
    fields::{Field, PrimeField},
    groups::Group,
    msm::VariableBaseMSM,
};
use ff_fft::polynomial::DensePolynomial as UnivariatePolynomial;

use std::marker::PhantomData;

use digest::Digest;
use rand::Rng;
use num_traits::identities::{Zero, One};

use crate::{
    tipa::{
        structured_scalar_message::{TIPAWithSSMProof},
        VerifierSRS, SRS, TIPA,
    },
    Error,
};
use dh_commitments::{
    identity::{HomomorphicPlaceholderValue, IdentityCommitment, IdentityOutput},
    afgho16::AFGHOCommitmentG1,
    pedersen::PedersenCommitment,
    DoublyHomomorphicCommitment,
};
use inner_products::{
    MultiexponentiationInnerProduct,
    ExtensionFieldElement,
};

type PolynomialEvaluationSecondTierIPA<P, D> = TIPA<
    MultiexponentiationInnerProduct<<P as PairingEngine>::G1Projective>,
    AFGHOCommitmentG1<P>,
    PedersenCommitment<<P as PairingEngine>::G1Projective>,
    IdentityCommitment<<P as PairingEngine>::G1Projective, <P as PairingEngine>::Fr>,
    P,
    D,
>;

type PolynomialEvaluationSecondTierIPAProof<P, D> = TIPAWithSSMProof<
    MultiexponentiationInnerProduct<<P as PairingEngine>::G1Projective>,
    AFGHOCommitmentG1<P>,
    PedersenCommitment<<P as PairingEngine>::G1Projective>,
    IdentityCommitment<<P as PairingEngine>::G1Projective, <P as PairingEngine>::Fr>,
    P,
    D,
>;

pub struct KZG<P: PairingEngine> {
    _pairing: PhantomData<P>,
}

// Simple implementation of KZG polynomial commitment scheme
impl<P: PairingEngine> KZG<P> {
    pub fn commit(
        powers: &[P::G1Affine],
        polynomial: &UnivariatePolynomial<P::Fr>,
    ) -> Result<P::G1Projective, Error> {
        assert!(powers.len() >= polynomial.degree() + 1);
        let mut coeffs = polynomial.coeffs.to_vec();
        coeffs.resize(powers.len(), <P::Fr>::zero());

        Ok(VariableBaseMSM::multi_scalar_mul(powers, &coeffs.iter().map(|b| b.into_repr()).collect::<Vec<_>>()))
    }

    pub fn open(
        powers: &[P::G1Affine],
        polynomial: &UnivariatePolynomial<P::Fr>,
        point: &P::Fr,
    ) -> Result<P::G1Projective, Error> {
        assert!(powers.len() >= polynomial.degree() + 1);

        // Trick to calculate (p(x) - p(z)) / (x - z) as p(x) / (x - z) ignoring remainder p(z)
        let quotient_polynomial = polynomial / &UnivariatePolynomial::from_coefficients_vec(vec![-point.clone(), P::Fr::one()]);
        let mut quotient_coeffs = quotient_polynomial.coeffs.to_vec();
        quotient_coeffs.resize(powers.len(), <P::Fr>::zero());
        Ok(VariableBaseMSM::multi_scalar_mul(powers, &quotient_coeffs.iter().map(|b| b.into_repr()).collect::<Vec<_>>()))
    }

    pub fn verify(
        v_srs: &VerifierSRS<P>,
        com: &P::G1Projective,
        point: &P::Fr,
        eval: &P::Fr,
        proof: &P::G1Projective,
    ) -> Result<bool, Error> {
        Ok(P::pairing(com.clone() - &<P::G1Projective as Group>::mul(&v_srs.g, eval), v_srs.h.clone())
            == P::pairing(proof.clone(), v_srs.h_alpha.clone() - &<P::G2Projective as Group>::mul(&v_srs.h, point)))
    }
}

pub struct BivariatePolynomial<F: Field> {
    y_polynomials: Vec<UnivariatePolynomial<F>>,
}

impl<F: Field> BivariatePolynomial<F> {
    pub fn evaluate(&self, point: &(F, F)) -> F {
        let (x, y) = point;
        let mut point_x_powers = vec![];
        let mut cur = F::one();
        for _ in 0..(self.y_polynomials.len()) {
            point_x_powers.push(cur);
            cur *= x;
        }
        point_x_powers.iter().zip(&self.y_polynomials)
            .map(|(x_power, y_polynomial)| x_power.clone() * y_polynomial.evaluate(y.clone()))
            .sum()
    }
}

pub struct OpeningProof<P: PairingEngine, D: Digest> {
    ip_proof: PolynomialEvaluationSecondTierIPAProof<P, D>,
    y_eval_comm: P::G1Projective,
    kzg_proof: P::G1Projective,
}

pub struct BivariatePolynomialCommitment<P: PairingEngine, D: Digest> {
    _pairing: PhantomData<P>,
    _digest: PhantomData<D>,
}

impl<P: PairingEngine, D: Digest> BivariatePolynomialCommitment<P, D> {
    pub fn setup<R: Rng>(rng: &mut R, x_degree: usize, y_degree: usize) -> Result<(SRS<P>, Vec<P::G1Affine>), Error> {
        //TODO: Fix when make TIPA with SSM less wasteful with second message
        //TODO: Fails when x_degree is smaller than half of y_degree because of kzg setup
        let srs = PolynomialEvaluationSecondTierIPA::<P, D>::setup(rng, x_degree + 1)?.0;
        let kzg_srs = <P as PairingEngine>::G1Projective::batch_normalization_into_affine(&srs.g_alpha_powers[0..y_degree + 1]);
        Ok((srs, kzg_srs))
    }

    pub fn commit(
        srs: &(SRS<P>, Vec<P::G1Affine>),
        bivariate_polynomial: &BivariatePolynomial<P::Fr>,
    ) -> Result<(ExtensionFieldElement<P>, Vec<P::G1Projective>), Error> {
        let (ip_srs, kzg_srs) = srs;
        let (ck, _) = ip_srs.get_commitment_keys();
        assert!(ck.len() >= bivariate_polynomial.y_polynomials.len());

        // Create KZG commitments to Y polynomials
        let y_polynomial_coms = bivariate_polynomial.y_polynomials.iter()
            .chain(vec![UnivariatePolynomial::zero()].iter().cycle())
            .take(ck.len())
            .map(|y_polynomial| {
                KZG::<P>::commit(kzg_srs, y_polynomial)
            }).collect::<Result<Vec<P::G1Projective>, Error>>()?;

        // Create AFGHO commitment to Y polynomial commitments
        Ok((AFGHOCommitmentG1::<P>::commit(&ck, &y_polynomial_coms)?, y_polynomial_coms))
    }

    pub fn open(
        srs: &(SRS<P>, Vec<P::G1Affine>),
        bivariate_polynomial: &BivariatePolynomial<P::Fr>,
        y_polynomial_comms: &Vec<P::G1Projective>,
        point: &(P::Fr, P::Fr),
    ) -> Result<OpeningProof<P, D>, Error> {
        let (x, y) = point;
        let (ip_srs, kzg_srs) = srs;
        let (ck_1, ck_2) = ip_srs.get_commitment_keys();
        assert!(ck_1.len() >= bivariate_polynomial.y_polynomials.len());

        let mut powers_of_x = vec![];
        let mut cur = P::Fr::one();
        for _ in 0..(ck_1.len()) {
            powers_of_x.push(cur);
            cur *= x;
        }

        let coeffs = bivariate_polynomial.y_polynomials.iter()
            .chain(vec![UnivariatePolynomial::zero()].iter().cycle())
            .take(ck_1.len())
            .map(|y_polynomial| {
                let mut c = y_polynomial.coeffs.to_vec();
                c.resize(kzg_srs.len(), <P::Fr>::zero());
                c
            }).collect::<Vec<Vec<P::Fr>>>();
        let y_eval_coeffs = (0..kzg_srs.len()).map(|j| {
            (0..ck_1.len()).map(|i| powers_of_x[i].clone() * &coeffs[i][j]).sum()
        }).collect::<Vec<P::Fr>>();
        let y_eval_comm = VariableBaseMSM::multi_scalar_mul(kzg_srs, &y_eval_coeffs.iter().map(|b| b.into_repr()).collect::<Vec<_>>());

        let ip_proof = PolynomialEvaluationSecondTierIPA::<P, D>::prove_with_structured_scalar_message(
            &ip_srs,
            (y_polynomial_comms, &powers_of_x),
            (&ck_1, &ck_2, &HomomorphicPlaceholderValue),
        )?;
        let kzg_proof = KZG::<P>::open(kzg_srs, &UnivariatePolynomial::from_coefficients_slice(&y_eval_coeffs), y)?;

        Ok(OpeningProof {
            ip_proof,
            y_eval_comm,
            kzg_proof,
        })
    }

    pub fn verify(
        v_srs: &VerifierSRS<P>,
        com: &ExtensionFieldElement<P>,
        point: &(P::Fr, P::Fr),
        eval: &P::Fr,
        proof: &OpeningProof<P, D>,
    ) -> Result<bool, Error> {
        let (x, y) = point;
        let ip_proof_valid = PolynomialEvaluationSecondTierIPA::<P, D>::verify_with_structured_scalar_message(
            v_srs,
            &HomomorphicPlaceholderValue,
            (com, &IdentityOutput(vec![proof.y_eval_comm.clone()])),
            x,
            &proof.ip_proof,
        )?;
        let kzg_proof_valid = KZG::<P>::verify(v_srs, &proof.y_eval_comm, y, eval, &proof.kzg_proof)?;
        Ok(ip_proof_valid && kzg_proof_valid)
    }
}

pub struct UnivariatePolynomialCommitment<P: PairingEngine, D: Digest> {
    _pairing: PhantomData<P>,
    _digest: PhantomData<D>,
}


impl<P: PairingEngine, D: Digest> UnivariatePolynomialCommitment<P, D> {
    fn bivariate_degrees(univariate_degree: usize) -> usize {
        (((univariate_degree + 1) as f64).sqrt().ceil() as usize).next_power_of_two() - 1
    }

    fn bivariate_form(bivariate_degree: usize, polynomial: &UnivariatePolynomial<P::Fr>) -> BivariatePolynomial<P::Fr> {
        let default_zero = vec![P::Fr::zero()];
        let mut coeff_iter = polynomial.coeffs.iter().chain(default_zero.iter().cycle())
            .take((bivariate_degree + 1).pow(2));

        let mut y_polynomials = Vec::new();
        for _ in 0..bivariate_degree + 1 {
            let mut y_polynomial_coeffs = vec![];
            for _ in 0..bivariate_degree + 1 {
                y_polynomial_coeffs.push(Clone::clone(coeff_iter.next().unwrap()))
            }
            y_polynomials.push(UnivariatePolynomial::from_coefficients_slice(&y_polynomial_coeffs));
        }
        BivariatePolynomial{y_polynomials}
    }

    pub fn setup<R: Rng>(rng: &mut R, degree: usize) -> Result<(SRS<P>, Vec<P::G1Affine>), Error> {
        //TODO: Fix when make TIPA with SSM less wasteful with second message
        //TODO: Fails when x_degree is smaller than half of y_degree because of kzg setup
        let bivariate_degree = Self::bivariate_degrees(degree);
        BivariatePolynomialCommitment::<P, D>::setup(rng, bivariate_degree, bivariate_degree)
    }

    pub fn commit(
        srs: &(SRS<P>, Vec<P::G1Affine>),
        polynomial: &UnivariatePolynomial<P::Fr>,
    ) -> Result<(ExtensionFieldElement<P>, Vec<P::G1Projective>), Error> {
        let bivariate_degree = srs.1.len() - 1;
        BivariatePolynomialCommitment::<P,D>::commit(srs, &Self::bivariate_form(bivariate_degree, polynomial))
    }

    pub fn open(
        srs: &(SRS<P>, Vec<P::G1Affine>),
        polynomial: &UnivariatePolynomial<P::Fr>,
        y_polynomial_comms: &Vec<P::G1Projective>,
        point: &P::Fr,
    ) -> Result<OpeningProof<P, D>, Error> {
        let bivariate_degree = srs.1.len() - 1;
        let y = point.clone();
        let x = point.pow(&vec![(bivariate_degree + 1) as u64]);
        BivariatePolynomialCommitment::open(srs, &Self::bivariate_form(bivariate_degree, polynomial), y_polynomial_comms, &(x, y))
    }

    pub fn verify(
        v_srs: &VerifierSRS<P>,
        bivariate_degree: usize,
        com: &ExtensionFieldElement<P>,
        point: &P::Fr,
        eval: &P::Fr,
        proof: &OpeningProof<P, D>,
    ) -> Result<bool, Error> {
        let y = point.clone();
        let x = y.pow(&vec![(bivariate_degree + 1) as u64]);
        BivariatePolynomialCommitment::verify(v_srs, com, &(x, y), eval, proof)
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use algebra::{bls12_381::Bls12_381, curves::PairingEngine, UniformRand};
    use blake2::Blake2b;
    use rand::{rngs::StdRng, SeedableRng};

    const BIVARIATE_X_DEGREE: usize = 7;
    const BIVARIATE_Y_DEGREE: usize = 7;
    const UNIVARIATE_DEGREE: usize = 56;

    type TestBivariatePolyCommitment = BivariatePolynomialCommitment<Bls12_381, Blake2b>;
    type TestUnivariatePolyCommitment = UnivariatePolynomialCommitment<Bls12_381, Blake2b>;

    #[test]
    fn bivariate_poly_commit_test() {
        let mut rng = StdRng::seed_from_u64(0u64);
        let srs = TestBivariatePolyCommitment::setup(&mut rng, BIVARIATE_X_DEGREE, BIVARIATE_Y_DEGREE).unwrap();
        let v_srs = srs.0.get_verifier_key();

        let mut y_polynomials = Vec::new();
        for _ in 0..BIVARIATE_X_DEGREE + 1 {
            let mut y_polynomial_coeffs = vec![];
            for _ in 0..BIVARIATE_Y_DEGREE + 1 {
                y_polynomial_coeffs.push(<Bls12_381 as PairingEngine>::Fr::rand(&mut rng));
            }
            y_polynomials.push(UnivariatePolynomial::from_coefficients_slice(&y_polynomial_coeffs));
        }
        let bivariate_polynomial = BivariatePolynomial{y_polynomials};

        // Commit to polynomial
        let (com, y_polynomial_comms) = TestBivariatePolyCommitment::commit(&srs, &bivariate_polynomial).unwrap();

        // Evaluate at challenge point
        let point = (UniformRand::rand(&mut rng), UniformRand::rand(&mut rng));
        let eval_proof = TestBivariatePolyCommitment::open(&srs, &bivariate_polynomial, &y_polynomial_comms, &point).unwrap();
        let eval = bivariate_polynomial.evaluate(&point);

        // Verify proof
        assert!(TestBivariatePolyCommitment::verify(&v_srs, &com, &point, &eval, &eval_proof).unwrap());
    }

    #[test]
    fn univariate_poly_commit_test() {
        let mut rng = StdRng::seed_from_u64(0u64);
        let srs = TestUnivariatePolyCommitment::setup(&mut rng, UNIVARIATE_DEGREE).unwrap();
        let v_srs = srs.0.get_verifier_key();

        let mut polynomial_coeffs = vec![];
        for _ in 0..UNIVARIATE_DEGREE + 1 {
            polynomial_coeffs.push(<Bls12_381 as PairingEngine>::Fr::rand(&mut rng));
        }
        let polynomial = UnivariatePolynomial::from_coefficients_slice(&polynomial_coeffs);

        // Commit to polynomial
        let (com, y_polynomial_comms) = TestUnivariatePolyCommitment::commit(&srs, &polynomial).unwrap();

        // Evaluate at challenge point
        let point = UniformRand::rand(&mut rng);
        let eval_proof = TestUnivariatePolyCommitment::open(&srs, &polynomial, &y_polynomial_comms, &point).unwrap();
        let eval = polynomial.evaluate(point.clone());

        // Verify proof
        assert!(TestUnivariatePolyCommitment::verify(&v_srs, BIVARIATE_X_DEGREE, &com, &point, &eval, &eval_proof).unwrap());
    }

}
