// Copyright (C) 2019-2022 Aleo Systems Inc.
// This file is part of the snarkVM library.

// The snarkVM library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkVM library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkVM library. If not, see <https://www.gnu.org/licenses/>.

use super::*;

use snarkvm_curves::{MontgomeryParameters, TwistedEdwardsParameters};

impl<E: Environment, const NUM_WINDOWS: usize, const WINDOW_SIZE: usize> BHPCRH<E, NUM_WINDOWS, WINDOW_SIZE> {
    pub fn hash(&self, input: &[Boolean<E>]) -> Field<E> {
        self.hash_bits_inner(input).to_x_coordinate()
    }

    fn hash_bits_inner(&self, input: &[Boolean<E>]) -> Group<E> {
        // Ensure the input size is at least the window size.
        if input.len() <= WINDOW_SIZE * BHP_CHUNK_SIZE {
            E::halt(format!("Inputs to this BHP variant must be greater than {} bits", WINDOW_SIZE * BHP_CHUNK_SIZE))
        }

        // Ensure the input size is within the parameter size.
        if input.len() > NUM_WINDOWS * WINDOW_SIZE * BHP_CHUNK_SIZE {
            E::halt(format!(
                "Inputs to this BHP variant cannot exceed {} bits",
                NUM_WINDOWS * WINDOW_SIZE * BHP_CHUNK_SIZE
            ))
        }

        // Pad the input to a multiple of `BHP_CHUNK_SIZE` for hashing.
        let mut input = input.to_vec();
        if input.len() % BHP_CHUNK_SIZE != 0 {
            let padding = BHP_CHUNK_SIZE - (input.len() % BHP_CHUNK_SIZE);
            input.extend_from_slice(&vec![Boolean::constant(false); BHP_CHUNK_SIZE][..padding]);
            assert_eq!(input.len() % BHP_CHUNK_SIZE, 0);
        }

        // Declare the 1/2 constant field element.
        let one_half = Field::constant(E::BaseField::half());

        // Declare the constant coefficients A and B for the Montgomery curve.
        let coeff_a = Field::constant(<E::AffineParameters as TwistedEdwardsParameters>::MontgomeryParameters::COEFF_A);
        let coeff_b = Field::constant(<E::AffineParameters as TwistedEdwardsParameters>::MontgomeryParameters::COEFF_B);

        // Implements the incomplete addition formulae of two Montgomery curve points.
        let montgomery_add = |(this_x, this_y): (&Field<E>, &Field<E>), (that_x, that_y): (&Field<E>, &Field<E>)| {
            // Construct `lambda` as a witness defined as:
            // `lambda := (that_y - this_y) / (that_x - this_x)`
            let lambda = witness!(|this_x, this_y, that_x, that_y| (that_y - this_y) / (that_x - this_x));

            // Ensure `lambda` is correct by enforcing:
            // `lambda * (that_x - this_x) == (that_y - this_y)`
            E::assert_eq(&lambda * (that_x - this_x), that_y - this_y);

            // Construct `sum_x` as a witness defined as:
            // `sum_x := (B * lambda^2) - A - this_x - that_x`
            let sum_x = witness!(|lambda, that_x, this_x, coeff_a, coeff_b| {
                coeff_b * lambda.square() - coeff_a - this_x - that_x
            });

            // Ensure `sum_x` is correct by enforcing:
            // `(B * lambda^2) == (A + this_x + that_x + sum_x)`
            E::assert_eq(&coeff_b * &lambda.square(), &coeff_a + this_x + that_x + &sum_x);

            // Construct `sum_y` as a witness defined as:
            // `sum_y := -(this_y + (lambda * (this_x - sum_x)))`
            let sum_y = witness!(|lambda, sum_x, this_x, this_y| -(this_y + (lambda * (sum_x - this_x))));

            // Ensure `sum_y` is correct by enforcing:
            // `(lambda * (this_x - sum_x)) == (this_y + sum_y)`
            E::assert_eq(lambda * (this_x - &sum_x), this_y + &sum_y);

            (sum_x, sum_y)
        };

        // Compute sum of h_i^{sum of (1-2*c_{i,j,2})*(1+c_{i,j,0}+2*c_{i,j,1})*2^{4*(j-1)} for all j in segment}
        // for all i. Described in section 5.4.1.7 in the Zcash protocol specification.
        //
        // Note: `.zip()` is used here (as opposed to `.zip_eq()`) as the input can be less than
        // `NUM_WINDOWS * WINDOW_SIZE * BHP_CHUNK_SIZE` in length, which is the parameter size here.
        input
            .chunks(WINDOW_SIZE * BHP_CHUNK_SIZE)
            .zip(self.bases.iter())
            .map(|(bits, bases)| {
                // Initialize accumulating sum variables for the x- and y-coordinates.
                let mut sum_x = Field::zero();
                let mut sum_y = Field::zero();

                // One iteration costs 2 constraints.
                bits.chunks(BHP_CHUNK_SIZE).zip(bases).for_each(|(chunk_bits, base)| {
                    let mut x_bases = Vec::with_capacity(4);
                    let mut y_bases = Vec::with_capacity(4);
                    let mut acc_power = base.clone();
                    for _ in 0..4 {
                        let x =
                            (Field::one() + acc_power.to_y_coordinate()) / (Field::one() - acc_power.to_y_coordinate());
                        let y = &x / acc_power.to_x_coordinate();

                        x_bases.push(x);
                        y_bases.push(y);
                        acc_power += base;
                    }

                    // Cast each input chunk bit as a field element.
                    let bit_0 = Field::from_boolean(&chunk_bits[0]);
                    let bit_1 = Field::from_boolean(&chunk_bits[1]);
                    let bit_2 = Field::from_boolean(&chunk_bits[2]);
                    let bit_0_and_1 = Field::from_boolean(&(&chunk_bits[0] & &chunk_bits[1])); // 1 constraint

                    // Compute the x-coordinate of the Montgomery curve point.
                    let montgomery_x: Field<E> = &x_bases[0]
                        + &bit_0 * (&x_bases[1] - &x_bases[0])
                        + &bit_1 * (&x_bases[2] - &x_bases[0])
                        + &bit_0_and_1 * (&x_bases[3] - &x_bases[2] - &x_bases[1] + &x_bases[0]);

                    // Compute the y-coordinate of the Montgomery curve point.
                    let montgomery_y = {
                        // Compute the y-coordinate of the Montgomery curve point, without any negation.
                        let y: Field<E> = &y_bases[0]
                            + bit_0 * (&y_bases[1] - &y_bases[0])
                            + bit_1 * (&y_bases[2] - &y_bases[0])
                            + bit_0_and_1 * (&y_bases[3] - &y_bases[2] - &y_bases[1] + &y_bases[0]);

                        // Determine the correct sign of the y-coordinate, as a witness.
                        //
                        // Instead of using `Field::ternary`, we create a witness & custom constraint to reduce
                        // the number of nonzero entries in the circuit, improving setup & proving time for Marlin.
                        let montgomery_y: Field<E> = witness!(|chunk_bits, y| if chunk_bits[2] { -y } else { y });

                        // Ensure the conditional negation of `witness_y` is correct as follows (1 constraint):
                        //     `(chunk_bits[2] - 1/2) * (-2 * y) == montgomery_y`
                        // which is equivalent to:
                        //     if `chunk_bits[2] == 0`, then `montgomery_y = -1/2 * -2 * y = y`
                        //     if `chunk_bits[2] == 1`, then `montgomery_y = 1/2 * -2 * y = -y`
                        E::enforce(|| (bit_2 - &one_half, -y.double(), &montgomery_y)); // 1 constraint

                        montgomery_y
                    };

                    // Sum the new Montgomery point into the accumulating sum.
                    (sum_x, sum_y) = montgomery_add((&sum_x, &sum_y), (&montgomery_x, &montgomery_y));
                });

                let edwards_x = &sum_x / sum_y; // 2 constraints
                let edwards_y = (&sum_x - Field::one()) / (sum_x + Field::one()); // 2 constraints
                Group::from_xy_coordinates(edwards_x, edwards_y) // 3 constraints
            })
            .fold(Group::zero(), |acc, group| acc + group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snarkvm_algorithms::{crh::BHPCRH as NativeBHP, CRH};
    use snarkvm_circuits_environment::Circuit;
    use snarkvm_curves::AffineCurve;
    use snarkvm_utilities::{test_rng, UniformRand};

    const ITERATIONS: usize = 10;
    const MESSAGE: &str = "BHPCircuit0";
    // const WINDOW_SIZE_MULTIPLIER: usize = 8;

    type Projective = <<Circuit as Environment>::Affine as AffineCurve>::Projective;

    fn check_hash<const NUM_WINDOWS: usize, const WINDOW_SIZE: usize>(
        mode: Mode,
        num_constants: usize,
        num_public: usize,
        num_private: usize,
        num_constraints: usize,
    ) {
        // Initialize the BHP hash.
        let native = NativeBHP::<Projective, NUM_WINDOWS, WINDOW_SIZE>::setup(MESSAGE);
        let circuit = BHPCRH::<Circuit, NUM_WINDOWS, WINDOW_SIZE>::setup(MESSAGE);
        // Determine the number of inputs.
        // let num_input_bits = NUM_WINDOWS * WINDOW_SIZE * BHP_CHUNK_SIZE;
        let num_input_bits = 128 * 8;

        for i in 0..ITERATIONS {
            // Sample a random input.
            let input = (0..num_input_bits).map(|_| bool::rand(&mut test_rng())).collect::<Vec<bool>>();
            // Compute the expected hash.
            let expected = native.hash(&input).expect("Failed to hash native input");
            // Prepare the circuit input.
            let circuit_input: Vec<Boolean<_>> = Inject::new(mode, input);

            Circuit::scope(format!("BHP {mode} {i}"), || {
                // Perform the hash operation.
                let candidate = circuit.hash(&circuit_input);
                assert_eq!(expected, candidate.eject_value());

                assert_scope!(num_constants, num_public, num_private, num_constraints);
            });
        }
    }

    // #[test]
    // fn test_hash_constant() {
    //     check_hash::<8, 32>(Mode::Constant);
    // }

    // #[test]
    // fn test_hash_public() {
    //     check_hash::<8, 32>(Mode::Public);
    // }

    #[test]
    fn test_hash_private() {
        check_hash::<32, 48>(Mode::Private, 41600, 0, 12669, 12701);
    }
}
