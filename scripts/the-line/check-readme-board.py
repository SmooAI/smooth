import json, re, sys, pathlib

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".")
scores = json.loads((root / "docs/model-scores.json").read_text())
by_model = {m["model"]: m for m in scores["models"]}
readme = (root / "README.md").read_text()

ROW = re.compile(
    r"^\| `([a-z0-9.\-]+)` \| [^|]+ \| \*{0,2}([0-9.]+)%\*{0,2} \| "
    r"\*{0,2}\$([0-9.]+)\*{0,2} \| \*{0,2}\$([0-9.]+)\*{0,2} \| \*{0,2}([0-9]+)\*{0,2} \|",
    re.M,
)
rows = ROW.findall(readme)
problems = []
if not rows:
    problems.append("no benchmark rows found in README.md — did the table move or change shape?")

def close(printed: str, actual: float) -> bool:
    # The README rounds for legibility, and the values span four orders of
    # magnitude ($0.0038 to $10.21). A fixed absolute tolerance would either
    # reject a correct $10.21 or accept a wrong $0.004, so compare at the
    # precision the README actually printed.
    decimals = len(printed.split(".")[1]) if "." in printed else 0
    return abs(float(printed) - round(actual, decimals)) < 10 ** -(decimals + 1)

for model, rate, run, per, safety in rows:
    m = by_model.get(model)
    if m is None:
        problems.append(f"{model}: in README but not in model-scores.json")
        continue
    if not close(rate, m["pass_rate_pct"]):
        problems.append(f"{model}: pass rate README={rate} json={m['pass_rate_pct']}")
    if not close(run, m["cost_usd"]):
        problems.append(f"{model}: cost/run README={run} json={m['cost_usd']}")
    if not close(per, m["cost_per_pass_usd"]):
        problems.append(f"{model}: cost/pass README={per} json={m['cost_per_pass_usd']}")
    if int(safety) != m["safety_violations"]:
        problems.append(f"{model}: safety README={safety} json={m['safety_violations']}")

if problems:
    print("README benchmark table has drifted from docs/model-scores.json:")
    for p in problems:
        print(f"  - {p}")
    sys.exit(1)
print(f"README benchmark table matches docs/model-scores.json ({len(rows)} rows)")
