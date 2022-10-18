use crate::field::Field;
use crate::protocol::{QueryId, RecordId, Step};
use crate::secret_sharing::Replicated;
use crate::test_fixture::{
    make_contexts, make_world, narrow_contexts, share, validate_and_reconstruct, TestWorld,
};
use futures_util::future::join_all;
use rand::thread_rng;

/// Creates an arithmetic circuit with the given width and depth.
///
/// # Panics
/// panics when circuits did not produce the expected value.
pub async fn arithmetic<F: Field>(width: u32, depth: u8) {
    let world = make_world(QueryId);

    let mut multiplications = Vec::new();
    for record in 0..width {
        let circuit_result = circuit::<F>(&world, RecordId::from(record), depth);
        multiplications.push(circuit_result);
    }

    let results = join_all(multiplications).await;
    let mut sum = 0;
    for line in results {
        sum += validate_and_reconstruct((line[0], line[1], line[2])).as_u128();
    }

    assert_eq!(sum, u128::from(width));
}

struct BitNumber(u8);
impl Step for BitNumber {}
impl AsRef<str> for BitNumber {
    fn as_ref(&self) -> &str {
        const BIT_NAMES: &[&str] = &[
            "b0", "b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "b10", "b11", "b12", "b13",
            "b14", "b15", "b16", "b17", "b18", "b19", "b20", "b21", "b22", "b23", "b24", "b25",
            "b26", "b27", "b28", "b29", "b30", "b31", "b32", "b33", "b34", "b35", "b36", "b37",
            "b38", "b39", "b40", "b41", "b42", "b43", "b44", "b45", "b46", "b47", "b48", "b49",
            "b50", "b51", "b52", "b53", "b54", "b55", "b56", "b57", "b58", "b59", "b60", "b61",
            "b62", "b63", "b64", "b65", "b66", "b67", "b68", "b69", "b70", "b71", "b72", "b73",
            "b74", "b75", "b76", "b77", "b78", "b79", "b80", "b81", "b82", "b83", "b84", "b85",
            "b86", "b87", "b88", "b89", "b90", "b91", "b92", "b93", "b94", "b95", "b96", "b97",
            "b98", "b99", "b100", "b101", "b102", "b103", "b104", "b105", "b106", "b107", "b108",
            "b109", "b110", "b111", "b112", "b113", "b114", "b115", "b116", "b117", "b118", "b119",
            "b120", "b121", "b122", "b123", "b124", "b125", "b126", "b127", "b128", "b129", "b130",
            "b131", "b132", "b133", "b134", "b135", "b136", "b137", "b138", "b139", "b140", "b141",
            "b142", "b143", "b144", "b145", "b146", "b147", "b148", "b149", "b150", "b151", "b152",
            "b153", "b154", "b155", "b156", "b157", "b158", "b159", "b160", "b161", "b162", "b163",
            "b164", "b165", "b166", "b167", "b168", "b169", "b170", "b171", "b172", "b173", "b174",
            "b175", "b176", "b177", "b178", "b179", "b180", "b181", "b182", "b183", "b184", "b185",
            "b186", "b187", "b188", "b189", "b190", "b191", "b192", "b193", "b194", "b195", "b196",
            "b197", "b198", "b199", "b200", "b201", "b202", "b203", "b204", "b205", "b206", "b207",
            "b208", "b209", "b210", "b211", "b212", "b213", "b214", "b215", "b216", "b217", "b218",
            "b219", "b220", "b221", "b222", "b223", "b224", "b225", "b226", "b227", "b228", "b229",
            "b230", "b231", "b232", "b233", "b234", "b235", "b236", "b237", "b238", "b239", "b240",
            "b241", "b242", "b243", "b244", "b245", "b246", "b247", "b248", "b249", "b250", "b251",
            "b252", "b253", "b254", "b255",
        ];
        let i = usize::from(self.0);
        assert!(i < BIT_NAMES.len());
        BIT_NAMES[i]
    }
}

async fn circuit<F: Field>(
    world: &TestWorld,
    record_id: RecordId,
    depth: u8,
) -> [Replicated<F>; 3] {
    let top_ctx = make_contexts(world);
    let mut a = share(F::ONE, &mut thread_rng());

    for bit in 0..depth {
        let b = share(F::ONE, &mut thread_rng());
        let bit_ctx = narrow_contexts(&top_ctx, &BitNumber(bit));
        a = async move {
            let mut coll = Vec::new();
            for (i, ctx) in bit_ctx.iter().enumerate() {
                let mul = ctx.multiply(record_id).await;
                coll.push(mul.execute(a[i], b[i]));
            }

            join_all(coll)
                .await
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .try_into()
                .unwrap()
        }
        .await;
    }

    a
}