/* UI replays captured evidence. It does not run agents or filesystem operations. */
const $ = (id) => document.getElementById(id);
let data,
  benchmarks,
  step = 0;
const stages = [
  [
    "FORK",
    "Same starting point.\nSeparate workspaces.",
    "Both tasks start with a configuration measured in seconds. Each has a private copy of the same revision.",
    "30 seconds",
    "The original default timeout.",
    "",
  ],
  [
    "WORK",
    "Both tests pass.\nBoth tasks look right.",
    "A changes the unit and preserves the default. B reads the old unit and writes a 60-second retry in a different file.",
    "2 / 2 pass",
    "Each task passes in its isolated workspace.",
    "good",
  ],
  [
    "MERGE",
    "A clean merge.\nA broken assumption.",
    "Git combines the edits without a conflict. The retry value is now interpreted in milliseconds. Its combined test fails.",
    "0.06 seconds",
    "The intended 60-second retry became 1,000× shorter.",
    "bad",
  ],
  [
    "VALIDATE",
    "The read is stale.\nKeep the work isolated.",
    "WitnessFS accepts the unit migration, then compares B’s observed configuration with revision 2. The content changed. B’s writes stay private.",
    "Rejected",
    "The receipt names /config.json and its mismatched hashes.",
    "bad",
  ],
  [
    "RERUN",
    "Read the new world.\nMake the right change.",
    "A fresh branch reads milliseconds and writes 60000. Validation accepts it. Both tests pass on the exported combined result.",
    "60 seconds",
    "Correct combined behavior at revision 3.",
    "good",
  ],
];
function outputs(commands) {
  return commands
    .map(
      (c) =>
        `$ ${c.label}\n${c.stdout}${c.stderr ? c.stderr : ""}[exit ${c.exit_code}]`,
    )
    .join("\n\n");
}
function showStep(n, scroll = false, updateHash = true) {
  if (!data) return;
  step = Math.max(0, Math.min(4, n));
  const [label, title, description, value, note, mood] = stages[step];
  $("stage-label").textContent = `STEP 0${step + 1} / ${label}`;
  $("stage-title").textContent = title;
  $("stage-title").style.whiteSpace = "pre-line";
  $("stage-description").textContent = description;
  $("outcome").className = `outcome ${mood}`;
  $("outcome").querySelector("b").textContent = value;
  $("outcome").querySelector("span").textContent = note;
  $("previous").disabled = step === 0;
  $("next").textContent = step === 4 ? "Replay from start ↻" : "Next step →";
  document
    .querySelectorAll("[data-step]")
    .forEach((b) =>
      b.setAttribute("aria-pressed", String(+b.dataset.step === step)),
    );
  $("dependency").classList.toggle("active", step >= 1);
  $("config-value").textContent =
    step === 0 ? "unit: seconds" : "unit: milliseconds";
  $("retry-value").textContent =
    step === 0
      ? "(not created yet)"
      : step === 4
        ? "timeout: 60000"
        : "timeout: 60";
  for (const id of ["config-test", "retry-test"]) {
    $(id).textContent =
      step === 0 ? "Isolated snapshot" : "✓ Isolated test passed";
    $(id).classList.toggle("pass", step > 0);
  }
  if (step === 4) $("retry-test").textContent = "✓ Fresh test passed";
  const g = data.git,
    w = data.witness;
  const base =
    'config.json\n{"timeout_unit": "seconds", "default_timeout": 30}\n\nBoth workspaces start here.';
  $("git-status").textContent = [
    "BASE",
    "TESTS PASS",
    "CLEAN MERGE / TEST FAILS",
    "STALE RESULT",
    "STALE RESULT",
  ][step];
  $("witness-status").textContent = [
    "BASE",
    "TESTS PASS",
    "WORK STILL PRIVATE",
    "STALE BRANCH REJECTED",
    "FRESH BRANCH ACCEPTED",
  ][step];
  $("git-output").textContent =
    step === 0
      ? base
      : step === 1
        ? outputs([...g.migrate, ...g.retry])
        : outputs([g.merge, ...g.combined_tests]);
  $("witness-output").textContent =
    step === 0
      ? base
      : step === 1
        ? outputs([...w.migrate, ...w.retry])
        : step === 2
          ? "The same two tasks pass in isolated FUSE mounts.\n\nNo changes are public until acceptance.\n\nNext: validate the observed dependencies."
          : step === 3
            ? JSON.stringify(
                {
                  branch: w.stale_receipt.branch,
                  base_revision: w.stale_receipt.base_revision,
                  checked_revision: w.stale_receipt.checked_revision,
                  outcome: w.stale_receipt.outcome,
                  conflicts: w.stale_receipt.conflicts.map((c) => ({
                    kind: c.kind,
                    path: c.path,
                    operation: c.operation,
                  })),
                },
                null,
                2,
              )
            : outputs([...w.fresh, ...w.final_tests]);
  if (updateHash) history.replaceState(null, "", `#replay-${step + 1}`);
  if (scroll)
    document.querySelector(".steps").scrollIntoView({ block: "start" });
}
const cases = {
  "missing-path": [
    "A file that wasn’t there.",
    "One task checks that feature.flag is absent and chooses “disabled”. Another creates the flag. The missing lookup was observed, so the stale decision is rejected.",
  ],
  "directory-phantom": [
    "A listing that became incomplete.",
    "One task enumerates plugins to build a catalog. Another adds a new plugin. No existing file changed, but the recorded directory membership did.",
  ],
  independent: [
    "Unrelated work still lands.",
    "The two tasks update separate files without reading each other’s inputs. Both branches are accepted, in sequence, from the same starting revision.",
  ],
  "conservative-rejection": [
    "A harmless comment still conflicts.",
    "The task only uses the first line of a file. Another task edits a comment. Whole-file observation rejects it anyway. This is a real false rejection, and an intentional precision tradeoff.",
  ],
};
function showCase(id, updateHash = true, scroll = false) {
  if (!data) return;
  const c = data.cases.find((c) => c.id === id),
    [title, description] = cases[id];
  $("case-title").textContent = title;
  $("case-description").textContent = description;
  $("case-verdict").textContent = c.reader.outcome.toUpperCase();
  $("case-verdict").style.color =
    c.reader.outcome === "accepted" ? "var(--green)" : "var(--red)";
  $("case-output").textContent = JSON.stringify(
    {
      writer: c.writer.outcome,
      reader: c.reader.outcome,
      observations_checked: c.reader.observations_checked,
      conflicts: c.reader.conflicts.map((x) => ({
        kind: x.kind,
        path: x.path,
      })),
    },
    null,
    2,
  );
  document
    .querySelectorAll("[data-case]")
    .forEach((b) =>
      b.setAttribute("aria-pressed", String(b.dataset.case === id)),
    );
  if (updateHash) history.replaceState(null, "", `#case-${id}`);
  if (scroll)
    document.querySelector(".case-heading").scrollIntoView({ block: "start" });
}

function restoreLocation() {
  const replay = location.hash.match(/^#replay-([1-5])$/);
  const caseId = location.hash.slice("#case-".length);
  if (replay) showStep(Number(replay[1]) - 1, true, false);
  else if (location.hash.startsWith("#case-") && Object.hasOwn(cases, caseId))
    showCase(caseId, false, true);
}
function chart(id, rows) {
  const root = $(id),
    max = Math.max(...rows.map((r) => r[1]));
  rows.forEach(([name, value, accent]) => {
    const row = document.createElement("div");
    row.className = "chart-row";
    const label = document.createElement("div");
    label.className = "chart-label";
    const nameEl = document.createElement("span");
    nameEl.textContent = name;
    const valueEl = document.createElement("b");
    valueEl.textContent = `${value.toFixed(2)} ms`;
    label.append(nameEl, valueEl);
    const track = document.createElement("div");
    track.className = "bar-track";
    const bar = document.createElement("div");
    bar.className = `bar ${accent ? "accent" : ""}`;
    bar.style.width = `${(value / max) * 100}%`;
    track.append(bar);
    row.append(label, track);
    root.append(row);
  });
}
function showBenchmarks() {
  const [small, large] = benchmarks.scales,
    io = benchmarks.io;
  chart("snapshot-chart", [
    ["Git worktree", large.git_worktree.median, false],
    ["WitnessFS snapshot", large.snapshot.median, true],
  ]);
  chart("read-chart", [
    ["Native filesystem", io.native_read.median, false],
    ["WitnessFS mount", io.witness_read.median, true],
  ]);
  $("snapshot-note").textContent =
    `25 snapshot samples, 8 Git worktree samples. The 100-file snapshot median was ${small.snapshot.median.toFixed(2)} ms. These operations have different materialization costs.`;
  $("read-note").textContent =
    `25 samples each. WitnessFS was ${(io.witness_read.median / io.native_read.median).toFixed(1)}× slower in this warm-read test. Observing inputs has a measurable cost.`;
  const rows = [
    ["One-time WitnessFS import", "import"],
    ["Git add + first commit", "git_add_and_commit"],
    ["WitnessFS mount + unmount", "mount_unmount"],
    ["Accept one new output file", "accept"],
  ];
  for (const [label, key] of rows) {
    const tr = document.createElement("tr");
    for (const val of [
      label,
      `${small[key].median.toFixed(2)} ms`,
      `${large[key].median.toFixed(2)} ms`,
    ]) {
      const td = document.createElement("td");
      td.textContent = val;
      tr.append(td);
    }
    $("benchmark-table").append(tr);
  }
}
document
  .querySelectorAll("[data-step]")
  .forEach((b) => b.addEventListener("click", () => showStep(+b.dataset.step)));
document
  .querySelectorAll("[data-case]")
  .forEach((b) => b.addEventListener("click", () => showCase(b.dataset.case)));
$("previous").addEventListener("click", () => showStep(step - 1));
$("next").addEventListener("click", () => showStep(step === 4 ? 0 : step + 1));
window.addEventListener("hashchange", restoreLocation);
Promise.all(
  ["demo", "benchmark", "kernel-tests"].map(async (name) => {
    const r = await fetch(`data/${name}.json`);
    if (!r.ok) throw new Error(`Cannot load ${name}`);
    return r.json();
  }),
)
  .then(([d, b, k]) => {
    data = d;
    benchmarks = b;
    $("receipt-json").textContent = JSON.stringify(
      d.witness.stale_receipt,
      null,
      2,
    );
    $("kernel-count").textContent = k.tests.length;
    showStep(0, false, false);
    showCase("missing-path", false);
    showBenchmarks();
    restoreLocation();
  })
  .catch((e) => {
    $("load-error").hidden = false;
    $("load-error").textContent =
      `The recorded evidence could not load. Reload this page to try again. For a local copy, serve the demo directory with python3 -m http.server. ${e.message}`;
    document.querySelectorAll("button").forEach((button) => { button.disabled = true; });
  });
