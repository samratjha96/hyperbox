#!/usr/bin/env bash
set -euo pipefail

HB_BIN="${HB_BIN:-./target/debug/hyperbox}"
SANDBOX_NAME="${SANDBOX_NAME:-agent-workload}"
WORKSPACE="${WORKSPACE:-/tmp/hyperbox-agent-workload}"
PROFILE="${PROFILE:-full}"
ROWS="${ROWS:-220000}"
TIMEOUT="${TIMEOUT:-1800}"
RESET=0
NO_INSTALL=0
DESTROY_AT_END=0

usage() {
  cat <<'EOF'
Run a realistic agent-style workload in Hyperbox.

What this script does:
1) Creates or reuses a named sandbox.
2) Installs multiple Python packages inside that sandbox.
3) Runs a longer data-analysis workflow.
4) Writes multiple artifacts to the mounted workspace.
5) Runs a second command to prove the environment is reusable.

Usage:
  scripts/agent_workload_stress.sh [options]

Options:
  --hb-bin <path>         Hyperbox CLI binary (default: ./target/debug/hyperbox)
  --name <name>           Sandbox affinity name (default: agent-workload)
  --workspace <path>      Host workspace mounted into sandbox (default: /tmp/hyperbox-agent-workload)
  --profile <name>        Hyperbox profile for create (default: full)
  --rows <n>              Number of synthetic rows to process (default: 220000)
  --timeout <sec>         Timeout per sandbox command (default: 1800)
  --reset                 Destroy existing named sandbox before running
  --no-install            Skip package install step (for warm reruns)
  --destroy-at-end        Destroy sandbox after workload completes
  -h, --help              Show this help

Environment variable overrides:
  HB_BIN, SANDBOX_NAME, WORKSPACE, PROFILE, ROWS, TIMEOUT
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --hb-bin)
      HB_BIN="$2"
      shift 2
      ;;
    --name)
      SANDBOX_NAME="$2"
      shift 2
      ;;
    --workspace)
      WORKSPACE="$2"
      shift 2
      ;;
    --profile)
      PROFILE="$2"
      shift 2
      ;;
    --rows)
      ROWS="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT="$2"
      shift 2
      ;;
    --reset)
      RESET=1
      shift
      ;;
    --no-install)
      NO_INSTALL=1
      shift
      ;;
    --destroy-at-end)
      DESTROY_AT_END=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ ! -x "$HB_BIN" ]]; then
  echo "Hyperbox binary is missing or not executable: $HB_BIN" >&2
  echo "Build it first: cargo build -p hyperbox-cli" >&2
  exit 1
fi

mkdir -p "$WORKSPACE"
mkdir -p "$WORKSPACE/out"

echo "[1/6] Preparing workload files in $WORKSPACE"
cat >"$WORKSPACE/requirements-agent-workload.txt" <<'EOF'
numpy
pandas
scikit-learn
matplotlib
pyarrow
requests
EOF

cat >"$WORKSPACE/agent_data_analysis.py" <<'PY'
#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
from sklearn.compose import ColumnTransformer
from sklearn.ensemble import RandomForestRegressor
from sklearn.metrics import mean_absolute_error, r2_score
from sklearn.model_selection import train_test_split
from sklearn.pipeline import Pipeline
from sklearn.preprocessing import OneHotEncoder


def build_dataset(rows: int) -> pd.DataFrame:
    rng = np.random.default_rng(20260309)
    event_time = pd.date_range("2025-01-01", periods=rows, freq="h")
    region = rng.choice(["na", "emea", "apac", "latam"], size=rows, p=[0.36, 0.28, 0.26, 0.10])
    channel = rng.choice(["search", "email", "social", "direct", "partner"], size=rows)
    device = rng.choice(["mobile", "desktop", "tablet"], size=rows, p=[0.59, 0.34, 0.07])
    ad_spend = rng.gamma(shape=2.3, scale=25.0, size=rows)
    page_views = rng.poisson(lam=7.5, size=rows).astype(float)
    session_seconds = rng.normal(loc=210.0, scale=75.0, size=rows).clip(20.0, None)
    discount_pct = rng.choice([0.0, 0.05, 0.10, 0.15, 0.20], size=rows, p=[0.45, 0.18, 0.2, 0.12, 0.05])

    missing_idx = rng.choice(rows, size=max(1, rows // 45), replace=False)
    page_views[missing_idx] = np.nan

    region_weight = {"na": 1.24, "emea": 1.09, "apac": 1.13, "latam": 0.91}
    channel_weight = {"search": 1.28, "email": 0.98, "social": 1.17, "direct": 0.88, "partner": 1.05}
    device_weight = {"mobile": 1.05, "desktop": 1.13, "tablet": 0.94}
    regional = np.array([region_weight[r] for r in region])
    channel_m = np.array([channel_weight[c] for c in channel])
    device_m = np.array([device_weight[d] for d in device])
    noise = rng.normal(0.0, 13.5, size=rows)

    revenue = (
        35.0
        + 0.39 * ad_spend
        + 2.1 * np.nan_to_num(page_views, nan=7.0)
        + 0.09 * session_seconds
        - 55.0 * discount_pct
    ) * regional * channel_m * device_m + noise

    return pd.DataFrame(
        {
            "event_time": event_time,
            "region": region,
            "channel": channel,
            "device": device,
            "ad_spend": ad_spend.round(3),
            "page_views": page_views,
            "session_seconds": session_seconds.round(3),
            "discount_pct": discount_pct.round(3),
            "revenue": revenue.round(3),
        }
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=220000)
    parser.add_argument("--output-dir", required=True)
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    started = time.time()
    raw = build_dataset(args.rows)
    raw.to_parquet(output_dir / "raw_events.parquet", index=False)
    raw.sample(min(len(raw), 30000), random_state=7).to_csv(output_dir / "raw_sample.csv", index=False)

    clean = raw.copy()
    clean["page_views"] = clean["page_views"].fillna(clean["page_views"].median())

    clean["week"] = clean["event_time"].dt.to_period("W").astype(str)
    weekly_revenue = clean.groupby("week", as_index=False)["revenue"].sum()
    weekly_revenue.to_csv(output_dir / "weekly_revenue.csv", index=False)

    clean["segment"] = clean["region"] + "-" + clean["channel"] + "-" + clean["device"]
    segment_summary = (
        clean.groupby("segment", as_index=False)
        .agg(
            rows=("segment", "count"),
            avg_revenue=("revenue", "mean"),
            avg_ad_spend=("ad_spend", "mean"),
            avg_session_seconds=("session_seconds", "mean"),
        )
        .sort_values("rows", ascending=False)
    )
    segment_summary.to_csv(output_dir / "segment_summary.csv", index=False)

    feature_cols = [
        "region",
        "channel",
        "device",
        "ad_spend",
        "page_views",
        "session_seconds",
        "discount_pct",
    ]
    target_col = "revenue"
    x = clean[feature_cols]
    y = clean[target_col]

    x_train, x_test, y_train, y_test = train_test_split(x, y, test_size=0.2, random_state=42)

    pre = ColumnTransformer(
        transformers=[
            ("categorical", OneHotEncoder(handle_unknown="ignore"), ["region", "channel", "device"]),
            ("numeric", "passthrough", ["ad_spend", "page_views", "session_seconds", "discount_pct"]),
        ]
    )

    model = Pipeline(
        steps=[
            ("prep", pre),
            ("regressor", RandomForestRegressor(n_estimators=180, max_depth=18, n_jobs=-1, random_state=42)),
        ]
    )
    model.fit(x_train, y_train)

    preds = model.predict(x_test)
    mae = float(mean_absolute_error(y_test, preds))
    r2 = float(r2_score(y_test, preds))

    eval_df = x_test.copy().reset_index(drop=True)
    eval_df["actual_revenue"] = y_test.reset_index(drop=True)
    eval_df["predicted_revenue"] = preds
    eval_df["abs_error"] = (eval_df["actual_revenue"] - eval_df["predicted_revenue"]).abs()
    eval_df.sort_values("abs_error", ascending=False).head(200).to_csv(
        output_dir / "top_prediction_errors.csv", index=False
    )

    plot_series = clean.set_index("event_time").resample("W")["revenue"].sum().tail(26)
    plt.figure(figsize=(12, 4))
    plot_series.plot(title="Weekly Revenue (Last 26 Weeks)")
    plt.xlabel("Week")
    plt.ylabel("Revenue")
    plt.tight_layout()
    plt.savefig(output_dir / "weekly_revenue.png", dpi=150)

    metrics = {
        "rows": int(args.rows),
        "train_rows": int(len(x_train)),
        "test_rows": int(len(x_test)),
        "mae": round(mae, 4),
        "r2": round(r2, 4),
        "artifacts": [
            "raw_events.parquet",
            "raw_sample.csv",
            "weekly_revenue.csv",
            "segment_summary.csv",
            "top_prediction_errors.csv",
            "weekly_revenue.png",
        ],
        "elapsed_seconds": round(time.time() - started, 2),
    }
    (output_dir / "metrics.json").write_text(json.dumps(metrics, indent=2) + "\n", encoding="utf-8")

    report = f"""# Hyperbox Agent Workload Report

- rows processed: {metrics["rows"]}
- train rows: {metrics["train_rows"]}
- test rows: {metrics["test_rows"]}
- MAE: {metrics["mae"]}
- R2: {metrics["r2"]}
- elapsed_seconds: {metrics["elapsed_seconds"]}

This run simulates an agent workflow:
- package installation in isolated environment
- non-trivial data processing and model training
- multi-format artifact generation for downstream use
"""
    (output_dir / "report.md").write_text(report, encoding="utf-8")

    print(json.dumps(metrics))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
PY

echo "[2/6] Resolving named sandbox"
if [[ $RESET -eq 1 ]]; then
  echo "Reset requested. Destroying sandbox name '$SANDBOX_NAME' if it exists."
  "$HB_BIN" destroy --name "$SANDBOX_NAME" >/dev/null 2>&1 || true
fi

if "$HB_BIN" list --json | python3 - "$SANDBOX_NAME" <<'PY' >/dev/null
import json
import sys

target = sys.argv[1]
for item in json.load(sys.stdin):
    if item.get("affinity_name") == target:
        raise SystemExit(0)
raise SystemExit(1)
PY
then
  echo "Reusing existing sandbox '$SANDBOX_NAME'."
else
  echo "Creating sandbox '$SANDBOX_NAME' with profile '$PROFILE'."
  "$HB_BIN" create \
    --name "$SANDBOX_NAME" \
    --profile "$PROFILE" \
    --workspace "$WORKSPACE" \
    --timeout "$TIMEOUT" \
    >/dev/null
fi

if [[ $NO_INSTALL -eq 0 ]]; then
  echo "[3/6] Installing multiple Python packages inside sandbox"
  "$HB_BIN" run \
    --name "$SANDBOX_NAME" \
    --timeout "$TIMEOUT" \
    --cmd "python3 -m pip install --disable-pip-version-check -r /workspace/requirements-agent-workload.txt"
else
  echo "[3/6] Skipping package install (--no-install)"
fi

echo "[4/6] Running heavy analysis script inside sandbox"
"$HB_BIN" run \
  --name "$SANDBOX_NAME" \
  --timeout "$TIMEOUT" \
  --cmd "python3 /workspace/agent_data_analysis.py --rows $ROWS --output-dir /workspace/out"

echo "[5/6] Re-run to verify environment persistence (no reinstall)"
VERIFY_CMD="$(cat <<'EOF'
python3 - <<'PY'
import json
import numpy
import pandas
import sklearn
from pathlib import Path

metrics_path = Path('/workspace/out/metrics.json')
metrics = json.loads(metrics_path.read_text(encoding='utf-8'))
print('versions', {'numpy': numpy.__version__, 'pandas': pandas.__version__, 'sklearn': sklearn.__version__})
print('metrics', {'rows': metrics['rows'], 'mae': metrics['mae'], 'r2': metrics['r2']})
PY
EOF
)"
"$HB_BIN" run --name "$SANDBOX_NAME" --timeout "$TIMEOUT" --cmd "$VERIFY_CMD"

echo "[6/6] Host-side artifact summary"
python3 - "$WORKSPACE" <<'PY'
import json
import sys
from pathlib import Path

workspace = Path(sys.argv[1]).resolve()
out = workspace / "out"
metrics = json.loads((out / "metrics.json").read_text(encoding="utf-8"))

print(f"workspace: {workspace}")
print(f"artifacts_dir: {out}")
for name in sorted(metrics["artifacts"] + ["metrics.json", "report.md"]):
    path = out / name
    if path.exists():
        print(f"- {name}: {path.stat().st_size} bytes")
print(f"model quality: mae={metrics['mae']} r2={metrics['r2']}")
print(f"elapsed_seconds: {metrics['elapsed_seconds']}")
PY

if [[ $DESTROY_AT_END -eq 1 ]]; then
  echo "Destroying sandbox '$SANDBOX_NAME' (--destroy-at-end)."
  "$HB_BIN" destroy --name "$SANDBOX_NAME" >/dev/null
else
  echo "Sandbox '$SANDBOX_NAME' remains running for reuse."
  echo "Destroy later with: $HB_BIN destroy --name $SANDBOX_NAME"
fi
