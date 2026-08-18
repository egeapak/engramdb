//! Measures the per-project cost EngramDB's machine-wide harvest fan-out pays:
//! LanceDB connect + open_table + flat k-NN, against a turbovec index holding
//! the same vectors. Mirrors the `conversations` table shape (2 vector columns
//! of 384 f32 + scalar columns).
use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray, UInt32Array};
use arrow_schema::{DataType, Field, Schema};
use futures_util::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::sync::Arc;
use std::time::Instant;
use turbovec::IdMapIndex;

const D: usize = 384;

fn vecs(n: usize, seed: u64) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(n * D);
    for _ in 0..n {
        let mut v: Vec<f32> = (0..D)
            .map(|i| {
                let (u1, u2): (f32, f32) = (rng.gen_range(1e-7..1.0), rng.gen_range(0.0..1.0));
                (-2.0 * u1.ln()).sqrt()
                    * (2.0 * std::f32::consts::PI * u2).cos()
                    * ((i + 1) as f32).powf(-0.5)
            })
            .collect();
        let nrm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= nrm;
        }
        out.extend_from_slice(&v);
    }
    out
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("session_id", DataType::Utf8, false),
        Field::new("project_id", DataType::Utf8, false),
        Field::new("cwd", DataType::Utf8, true),
        Field::new("git_branch", DataType::Utf8, true),
        Field::new("first_prompt", DataType::Utf8, true),
        Field::new("summary", DataType::Utf8, true),
        Field::new("started_at", DataType::Utf8, true),
        Field::new("ended_at", DataType::Utf8, true),
        Field::new("user_turns", DataType::UInt32, true),
        Field::new(
            "digest_vec",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                D as i32,
            ),
            false,
        ),
        Field::new(
            "summary_vec",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                D as i32,
            ),
            true,
        ),
    ]))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join("lanceprobe-db");
    let _ = std::fs::remove_dir_all(&base);

    println!("per-project search cost: LanceDB (as EngramDB uses it) vs turbovec, d=384, k=10\n");
    println!(
        "{:>6} | {:>12} | {:>12} | {:>12} | {:>12} | {:>12}",
        "rows", "lance cold", "lance warm", "tv load+q", "tv warm q", "tv file"
    );
    println!("{}", "-".repeat(84));

    for &n in &[4usize, 100, 500, 2000, 5000] {
        let dir = base.join(format!("p{n}"));
        std::fs::create_dir_all(&dir)?;
        let data = vecs(n, 1);
        let q = vecs(1, 9);

        // --- build the Lance table ---
        let sch = schema();
        let sid: Vec<String> = (0..n)
            .map(|i| format!("session-{i:08}-uuid-like-000000000000"))
            .collect();
        let txt: Vec<Option<String>> = (0..n)
            .map(|i| Some(format!("some recorded prose for row {i}")))
            .collect();
        let fsl = |src: &[f32]| {
            FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                (0..n).map(|i| {
                    Some(
                        src[i * D..(i + 1) * D]
                            .iter()
                            .map(|x| Some(*x))
                            .collect::<Vec<_>>(),
                    )
                }),
                D as i32,
            )
        };
        let batch = RecordBatch::try_new(
            sch.clone(),
            vec![
                Arc::new(StringArray::from(sid.clone())),
                Arc::new(StringArray::from(vec!["proj"; n])),
                Arc::new(StringArray::from(txt.clone())),
                Arc::new(StringArray::from(txt.clone())),
                Arc::new(StringArray::from(txt.clone())),
                Arc::new(StringArray::from(txt.clone())),
                Arc::new(StringArray::from(vec![Some("2026-01-01T00:00:00Z"); n])),
                Arc::new(StringArray::from(vec![Some("2026-01-01T01:00:00Z"); n])),
                Arc::new(UInt32Array::from(vec![Some(5u32); n])),
                Arc::new(fsl(&data)),
                Arc::new(fsl(&data)),
            ],
        )?;
        {
            let conn = lancedb::connect(dir.to_str().unwrap()).execute().await?;
            let reader = RecordBatchIterator::new(vec![Ok(batch.clone())], sch.clone());
            conn.create_table("conversations", Box::new(reader))
                .execute()
                .await?;
        }

        // --- LanceDB cold: fresh connect + open_table + 2 column searches (the fan-out shape) ---
        let reps = if n > 2000 { 8 } else { 20 };
        let mut warm = None;
        let t = Instant::now();
        for _ in 0..reps {
            let conn = lancedb::connect(dir.to_str().unwrap()).execute().await?;
            let tbl = conn.open_table("conversations").execute().await?;
            for col in ["digest_vec", "summary_vec"] {
                let _: Vec<RecordBatch> = tbl
                    .query()
                    .nearest_to(q.clone())?
                    .column(col)
                    .limit(10)
                    .execute()
                    .await?
                    .try_collect()
                    .await?;
            }
        }
        let lance_cold = t.elapsed() / reps;

        // --- LanceDB warm: connection + table handle reused, only the queries repeat ---
        {
            let conn = lancedb::connect(dir.to_str().unwrap()).execute().await?;
            let tbl = conn.open_table("conversations").execute().await?;
            for _ in 0..3 {
                let _: Vec<RecordBatch> = tbl
                    .query()
                    .nearest_to(q.clone())?
                    .column("digest_vec")
                    .limit(10)
                    .execute()
                    .await?
                    .try_collect()
                    .await?;
            }
            let t = Instant::now();
            for _ in 0..reps {
                for col in ["digest_vec", "summary_vec"] {
                    let _: Vec<RecordBatch> = tbl
                        .query()
                        .nearest_to(q.clone())?
                        .column(col)
                        .limit(10)
                        .execute()
                        .await?
                        .try_collect()
                        .await?;
                }
            }
            warm = Some(t.elapsed() / reps);
        }

        // --- turbovec: two indexes (one per column), written to disk ---
        let ids: Vec<u64> = (0..n as u64).collect();
        let mut idx = IdMapIndex::new(D, 4).unwrap();
        idx.add_with_ids_2d(&data, D, &ids).unwrap();
        let path = dir.join("digest.tvim");
        idx.write(path.to_str().unwrap()).unwrap();
        let tv_bytes = std::fs::metadata(&path)?.len();

        let t = Instant::now();
        for _ in 0..reps {
            let mut a = IdMapIndex::load(path.to_str().unwrap()).unwrap();
            let mut b = IdMapIndex::load(path.to_str().unwrap()).unwrap();
            std::hint::black_box(a.search(&q, 10));
            std::hint::black_box(b.search(&q, 10));
        }
        let tv_cold = t.elapsed() / reps;

        let mut a = IdMapIndex::load(path.to_str().unwrap()).unwrap();
        let mut b = IdMapIndex::load(path.to_str().unwrap()).unwrap();
        a.prepare();
        b.prepare();
        for _ in 0..20 {
            let _ = a.search(&q, 10);
        }
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(a.search(&q, 10));
            std::hint::black_box(b.search(&q, 10));
        }
        let tv_warm = t.elapsed() / reps;

        println!(
            "{:>6} | {:>12.2?} | {:>12.2?} | {:>12.2?} | {:>12.2?} | {:>10} KB",
            n,
            lance_cold,
            warm.unwrap(),
            tv_cold,
            tv_warm,
            tv_bytes / 1024
        );
    }
    Ok(())
}
