#!/bin/bash
# Retrain Laya on train + supplement with three seeds, scoring each checkpoint on every set.
# Expects $WORK_DIR/laya-venv (pip install laya==0.4.8 + CPU torch) and
# $WORK_DIR/laya-data/aug/{train.jsonl,val.json} from split_gate_data.py + to_laya_jsonl.py.
# Probabilities land in $WORK_DIR; score them with gate_threshold_report.py.
# Set SEEDS to override the default "0 1 2".
set -u
S=${WORK_DIR:?set WORK_DIR to a scratch directory holding laya-venv, laya-data and scripts}
D=${DATA_DIR:-$(cd "$(dirname "$0")/../../examples/data" && pwd)}
TOOLS=$(cd "$(dirname "$0")" && pwd)
cd "$S"
export HF_HOME=${HF_HOME:-$S/hf-home} OMP_NUM_THREADS=${OMP_NUM_THREADS:-4}
SETS="laya-data/aug/val.json $D/prompt_turn_eval.json $D/prompt_turn_test_realistic.json $D/prompt_turn_test_hard.json $D/prompt_turn_test_hard2.json"
for seed in ${SEEDS:-0 1 2}; do
  echo "[$(date -u +%T)] train seed $seed"
  start=$(date +%s)
  laya-venv/bin/laya-train --data laya-data/aug/train.jsonl \
    --base convaiinnovations/laya --out laya-ckpt-aug-s$seed --epochs 4 --micro-batch 8 --grad-accum 1 \
    --device cpu --seed $seed --max-len 256 2>&1 | grep -E 'epoch|Accuracy|complete|Error|error'
  echo "train seed $seed: $(( $(date +%s) - start )) s" 
  echo "[$(date -u +%T)] eval seed $seed"
  laya-venv/bin/python -I "$TOOLS"/laya_gate_eval.py laya-ckpt-aug-s$seed $SETS 2>&1 | grep -E '^prompt_turn|^val|P\(work\)'
done
echo "[$(date -u +%T)] DONE"
