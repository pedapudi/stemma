/* stemma eval report — standalone renderer.
 *
 * Reads the run JSON injected by stemma-eval into #run-data and renders the
 * full report: header, the mechanism × tier matrix (recall@5 with delta and
 * CI per cell, click-through per-query lists), NIL panel, calibration
 * curves (inline SVG), cost tables, and named failures.
 *
 * The chrome (16-theme color picker, 12-face typeface picker, size control)
 * is COPIED from ui/src/ui.ts, not imported, so a generated report stays
 * self-contained with zero external requests. localStorage keys match the
 * console's, so a reader's theme follows them here.
 */

/* ---------- run file types (mirror crates/stemma-eval/src/runner.rs) ---- */

interface Delta {
  vs: string;
  mean: number;
  ci: [number, number];
  p: number;
  n: number;
}

interface QueryBrief {
  id: string;
  question: string;
  r5: number;
  rinf: number;
  mrr: number;
  grounded: boolean;
  nil_outcome: boolean;
  note: string;
}

interface CellReport {
  n: number;
  n_targets: number;
  r1_loose: number;
  r5_loose: number;
  rinf_loose: number;
  r1_strict: number;
  r5_strict: number;
  rinf_strict: number;
  mrr: number;
  grounded: number;
  mention_f_strict_micro: number;
  mention_f_weak_micro: number;
  mention_f_strict_macro: number;
  mention_f_weak_macro: number;
  latency_median_ms: number;
  latency_p95_ms: number;
  dense_probes_mean: number;
  adjudication_rate: number;
  selected_per_mention: number;
  delta_prev: Delta | null;
  delta_baseline: Delta | null;
  queries: QueryBrief[];
}

interface ConfidentWrong {
  id: string;
  question: string;
  candidate: string;
  score: number;
}

interface NilReport {
  precision: number | null;
  recall: number | null;
  confident_wrong: ConfidentWrong[];
}

interface CalibrationBucket {
  lo: number;
  hi: number;
  n: number;
  p_gold: number;
}

interface BackendCost {
  embed_calls: number;
  embed_texts: number;
  embed_ms_total: number;
  lm_calls: number;
  lm_ms_mean: number;
}

interface TukeyPair {
  a: string;
  b: string;
  p: number;
}

interface Failure {
  check: string;
  cell: string;
  detail: string;
  queries: string[];
}

interface RunFile {
  run_id: string;
  corpus: string;
  dataset: string;
  git_rev: string;
  date: string;
  ablations: string[];
  tiers: string[];
  cells: Record<string, Record<string, CellReport>>;
  nil: Record<string, NilReport>;
  calibration: Record<string, CalibrationBucket[]>;
  backend_cost: Record<string, BackendCost>;
  tukey: Record<string, TukeyPair[]>;
  pass: boolean | null;
  failures: Failure[];
  notes: string[];
}

/* ---------- DOM helpers (console idiom) ---------- */

type Child = Node | string | number | null | undefined | Child[];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs?: Record<string, unknown> | null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === null || v === undefined) continue;
    if (k === "class") n.className = String(v);
    else if (k.startsWith("on") && typeof v === "function") {
      n.addEventListener(k.slice(2), v as EventListener);
    } else n.setAttribute(k, String(v));
  }
  appendChildren(n, children);
  return n;
}

function svgEl(tag: string, attrs?: Record<string, unknown> | null, ...children: Child[]): SVGElement {
  const n = document.createElementNS("http://www.w3.org/2000/svg", tag) as SVGElement;
  for (const [k, v] of Object.entries(attrs ?? {})) n.setAttribute(k, String(v));
  appendChildren(n, children);
  return n;
}

function appendChildren(n: Element, children: Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined) continue;
    if (Array.isArray(c)) appendChildren(n, c);
    else n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

/* ---------- chrome: theme + typeface pickers (copied from ui/src/ui.ts) -- */

const COLOR_THEMES: [string, string, [string, string, string, string, string, string]][] = [
  ["monokai", "monokai", ["#1e1f1c", "#272822", "#f8f8f2", "#a6e22e", "#f92672", "#66d9ef"]],
  ["solarized-dark", "solarized dark", ["#04222B", "#0A2D38", "#93A1A1", "#8BB80E", "#E0483C", "#2AA198"]],
  ["solarized-light", "solarized light", ["#FDF6E3", "#FBF1D6", "#586E75", "#6B9B0B", "#DC322F", "#268BD2"]],
  ["google-light", "google light", ["#FFFFFF", "#F4F4F4", "#474A4E", "#34A853", "#EA4335", "#1B9CB8"]],
  ["google-dark", "google dark", ["#202124", "#2C2D30", "#FFFFFF", "#34A853", "#EA4335", "#24C1E0"]],
  ["lunaria-light", "lunaria light", ["#EBE4E1", "#E2DCD9", "#363434", "#497D46", "#783C1F", "#3778A9"]],
  ["lunaria-eclipse", "lunaria eclipse", ["#323F46", "#3B484F", "#DFE2ED", "#BEDBC1", "#BA9088", "#C8429F"]],
  ["belafonte-day", "belafonte day", ["#D5CCBA", "#CCC3B2", "#34292D", "#6E6A4E", "#BE100E", "#426A79"]],
  ["belafonte-night", "belafonte night", ["#20111B", "#271821", "#D5CCBA", "#A6A07A", "#D6403E", "#6F8E97"]],
  ["paper", "paper", ["#F2EEDE", "#E6E2D3", "#1A1A1A", "#216609", "#CC3E28", "#1E6FCC"]],
  ["zenburn", "zenburn", ["#3A3A3A", "#424241", "#DCDCCC", "#8FB28F", "#CC9393", "#8CD0D3"]],
  ["selenized-black", "selenized black", ["#181818", "#202020", "#DEDEDE", "#83C746", "#FF5E56", "#56D8C9"]],
  ["relaxed", "relaxed", ["#353A44", "#3D424B", "#F7F7F7", "#A0AC77", "#BC5653", "#7EAAC7"]],
  ["espresso", "espresso", ["#323232", "#3A3A3A", "#FFFFFF", "#A5C261", "#D25252", "#6C99BB"]],
  ["dracula", "dracula", ["#282A36", "#343746", "#F8F8F2", "#50FA7B", "#FF5555", "#BD93F9"]],
  ["ubuntu", "ubuntu", ["#300A24", "#3D1530", "#EEEEEC", "#8AE234", "#CC0000", "#34E2E2"]],
];

const MONO_GSM = '"Google Sans Mono","Noto Sans Mono",ui-monospace,monospace';
const SANS_GROTESK = '"Space Grotesk",system-ui,sans-serif';

interface TypeOption {
  id: string;
  label: string;
  group: string;
  head: string;
  body: string;
}

const TYPE_OPTIONS: TypeOption[] = [
  { id: "T7", label: "Google Sans Mono", group: "technical", head: MONO_GSM, body: MONO_GSM },
  { id: "T9", label: "Source Sans 3 + Source Code Pro", group: "technical",
    head: '"Source Sans 3",system-ui,sans-serif', body: '"Source Sans 3",system-ui,sans-serif' },
  { id: "T12", label: "Inconsolata", group: "technical",
    head: '"Inconsolata",ui-monospace,monospace', body: '"Inconsolata",ui-monospace,monospace' },
  { id: "T14", label: "Ubuntu + Ubuntu Mono", group: "technical",
    head: '"Ubuntu",system-ui,sans-serif', body: '"Ubuntu",system-ui,sans-serif' },
  { id: "E5", label: "Fraunces", group: "editorial",
    head: '"Fraunces",Georgia,serif', body: '"Fraunces",Georgia,serif' },
  { id: "E7", label: "Bitter", group: "editorial",
    head: '"Bitter",Georgia,serif', body: '"Bitter",Georgia,serif' },
  { id: "E8", label: "Literata", group: "editorial",
    head: '"Literata",Georgia,serif', body: '"Literata",Georgia,serif' },
  { id: "E15", label: "Domine", group: "editorial",
    head: '"Domine",Georgia,serif', body: '"Domine",Georgia,serif' },
  { id: "D2", label: "Archivo Narrow + Space Grotesk", group: "display",
    head: `"Archivo Narrow",${SANS_GROTESK}`, body: SANS_GROTESK },
  { id: "D12", label: "Hanken Grotesk", group: "display",
    head: '"Hanken Grotesk",system-ui,sans-serif', body: '"Hanken Grotesk",system-ui,sans-serif' },
  { id: "D14", label: "Barlow Condensed + Space Grotesk", group: "display",
    head: '"Barlow Condensed","Archivo Narrow",system-ui,sans-serif', body: SANS_GROTESK },
  { id: "D5", label: "Bricolage Grotesque", group: "display",
    head: '"Bricolage Grotesque",system-ui,sans-serif', body: '"Bricolage Grotesque",system-ui,sans-serif' },
];
const SAMPLE_LINE = "the quick brown fox 0123";
const FONT_SIZES: [string, number][] = [["s", 1], ["m", 1.15], ["l", 1.3]];

const openMenus: HTMLElement[] = [];

function closeAllMenus(except?: Element): void {
  for (const m of openMenus) {
    if (m !== except) m.classList.remove("open");
  }
}

function swatchStrip(colors: string[]): HTMLElement {
  const strip = el("span", { class: "swatch-strip", "aria-hidden": "true" });
  for (const c of colors) {
    const chip = el("i");
    chip.style.background = c;
    strip.append(chip);
  }
  return strip;
}

/* file:// contexts can deny storage entirely; a report must render
 * anywhere, so storage is best-effort. */
function store(key: string): string | null {
  try { return store(key); } catch { return null; }
}
function storeSet(key: string, v: string): void {
  try { storeSet(key, v); } catch { /* view-only context */ }
}

function buildThemePicker(): void {
  const mount = document.getElementById("themepicker") as HTMLElement;
  const saved = store("stemma.theme") ?? "paper";
  document.documentElement.dataset.theme = saved;
  const current = COLOR_THEMES.find((t) => t[0] === saved) ?? COLOR_THEMES[9];

  const trigName = el("span", { class: "cd-name" }, current[1]);
  const trigStrip = el("span", null, swatchStrip(current[2]));
  const list = el("div", { class: "cd-list", role: "listbox" });
  const cd = el("span", { class: "cd" },
    el("button", {
      class: "cd-trigger",
      onclick: (e: Event) => {
        e.stopPropagation();
        closeAllMenus(cd);
        cd.classList.toggle("open");
      },
    }, trigStrip, trigName, el("span", { class: "cd-caret" }, "▾")),
    list);

  for (const [id, label, colors] of COLOR_THEMES) {
    const opt = el("div", {
      class: "cd-option",
      role: "option",
      "aria-selected": id === saved ? "true" : "false",
      onclick: () => {
        document.documentElement.dataset.theme = id;
        storeSet("stemma.theme", id);
        list.querySelectorAll(".cd-option").forEach((o) => o.setAttribute("aria-selected", "false"));
        opt.setAttribute("aria-selected", "true");
        trigName.textContent = label;
        trigStrip.replaceChildren(swatchStrip(colors));
        cd.classList.remove("open");
      },
    }, swatchStrip(colors), el("span", { class: "cd-name" }, label));
    list.append(opt);
  }
  openMenus.push(cd);
  mount.append(cd);
}

function buildTypePicker(): void {
  const mount = document.getElementById("typepicker") as HTMLElement;
  const saved = store("stemma.type") ?? "T9";
  if (saved !== "T9") document.documentElement.dataset.type = saved;
  const savedSize = store("stemma.fontsize") ?? "s";
  const sizeVal = FONT_SIZES.find(([k]) => k === savedSize)?.[1] ?? 1;
  if (sizeVal !== 1) document.documentElement.style.setProperty("--fs", String(sizeVal));
  const current = TYPE_OPTIONS.find((o) => o.id === saved) ?? TYPE_OPTIONS[1];

  const trigName = el("span", { class: "cd-name" }, current.label);
  const trigSpec = el("span", { class: "tf-spec" }, "Ag");
  trigSpec.style.fontFamily = current.head;
  const listBox = el("div", { class: "tf-list" });
  const pop = el("div", { class: "tf-pop" }, listBox);
  const cd = el("span", { class: "cd" },
    el("button", {
      class: "cd-trigger",
      onclick: (e: Event) => {
        e.stopPropagation();
        closeAllMenus(cd);
        cd.classList.toggle("open");
      },
    }, trigSpec, trigName, el("span", { class: "cd-caret" }, "▾")),
    pop);

  for (const group of ["technical", "editorial", "display"]) {
    listBox.append(el("div", { class: "tf-group" }, el("div", { class: "subhead" }, group)));
    for (const o of TYPE_OPTIONS.filter((x) => x.group === group)) {
      const name = el("span", { class: "tf-name" }, o.label);
      name.style.fontFamily = o.head;
      const sample = el("span", { class: "tf-sample", "aria-hidden": "true" }, SAMPLE_LINE);
      sample.style.fontFamily = o.body;
      const opt = el("div", {
        class: "tf-option",
        role: "option",
        "aria-selected": o.id === saved ? "true" : "false",
        onclick: () => {
          if (o.id === "T9") delete document.documentElement.dataset.type;
          else document.documentElement.dataset.type = o.id;
          storeSet("stemma.type", o.id);
          pop.querySelectorAll(".tf-option").forEach((x) => x.setAttribute("aria-selected", "false"));
          opt.setAttribute("aria-selected", "true");
          trigName.textContent = o.label;
          trigSpec.style.fontFamily = o.head;
        },
      }, name, sample);
      listBox.append(opt);
    }
  }

  const seg = el("span", { class: "seg" });
  for (const [k, v] of FONT_SIZES) {
    const b = el("button", {
      class: k === savedSize ? "on" : "",
      onclick: () => {
        document.documentElement.style.setProperty("--fs", String(v));
        storeSet("stemma.fontsize", k);
        seg.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
        b.classList.add("on");
      },
    }, k);
    seg.append(b);
  }
  pop.append(el("div", { class: "tf-foot" }, el("span", { class: "k" }, "text size"), seg));

  openMenus.push(cd);
  mount.append(cd);
}

/* ---------- formatting ---------- */

const f3 = (x: number): string => x.toFixed(3);
const pct = (x: number): string => (100 * x).toFixed(1);
const signed = (x: number): string => (x >= 0 ? "+" : "") + (100 * x).toFixed(1);

/* ---------- sections ---------- */

function header(run: RunFile): HTMLElement {
  const pass = run.pass === null
    ? el("span", { class: "pill neutral" }, "ungraded — no baseline")
    : run.pass
      ? el("span", { class: "pill good" }, el("span", { class: "dot good" }), "pass")
      : el("span", { class: "pill bad" }, el("span", { class: "dot bad" }), "fail");
  return el("div", { class: "section" },
    el("div", { class: "runhead" },
      el("span", { class: "h1" }, "evaluation run — ", run.corpus),
      pass),
    el("div", { class: "kv" },
      el("span", { class: "k" }, "run id"), el("span", { class: "v" }, run.run_id),
      el("span", { class: "k" }, "git rev"), el("span", { class: "v" }, run.git_rev),
      el("span", { class: "k" }, "date (utc)"), el("span", { class: "v" }, run.date),
      el("span", { class: "k" }, "dataset"), el("span", { class: "v" }, run.dataset),
      el("span", { class: "k" }, "ablations"), el("span", { class: "v" }, run.ablations.join(" → "))));
}

function cellDelta(c: CellReport): Delta | null {
  return c.delta_baseline ?? c.delta_prev;
}

function matrixSection(run: RunFile): HTMLElement {
  const drawer = el("div", { class: "drawer" });
  let openKey = "";

  const table = el("table", { class: "grid matrix" });
  const head = el("tr", null, el("th", null, "mechanism"));
  for (const tier of run.tiers) head.append(el("th", null, tier));
  table.append(head);

  for (const ab of run.ablations) {
    const row = el("tr", null, el("td", { class: "mono" }, ab));
    for (const tier of run.tiers) {
      const c = run.cells[ab]?.[tier];
      if (!c) {
        row.append(el("td", null, el("div", { class: "cellbox", disabled: "true" },
          el("span", { class: "cell-r5 faint" }, "—"))));
        continue;
      }
      const d = cellDelta(c);
      const key = `${ab}×${tier}`;
      const box = el("button", { class: "cellbox", onclick: () => {
        if (openKey === key) {
          openKey = "";
          drawer.replaceChildren();
          box.classList.remove("on");
          return;
        }
        openKey = key;
        table.querySelectorAll(".cellbox.on").forEach((b) => b.classList.remove("on"));
        box.classList.add("on");
        drawer.replaceChildren(cellDrawer(ab, tier, c));
      } },
        el("span", { class: "cell-r5" }, f3(c.r5_strict)),
        d
          ? el("span", { class: "cell-delta " + (d.mean > 0.0005 ? "up" : d.mean < -0.0005 ? "down" : "") },
              `${signed(d.mean)} (${d.vs.replace("prev:", "vs ")})`)
          : el("span", { class: "cell-delta" }, "n=" + c.n),
        d ? el("span", { class: "cell-ci" },
              `ci [${signed(d.ci[0])}, ${signed(d.ci[1])}] p=${d.p.toFixed(3)}`) : null);
      row.append(el("td", null, box));
    }
    table.append(row);
  }

  return el("div", { class: "section" },
    el("div", { class: "h2" }, "mechanism × tier matrix"),
    el("div", { class: "lede" },
      "column-strict recall@5 per cell; deltas vs the accepted baseline where one exists, ",
      "else vs the previous ablation. click a cell for its per-query list."),
    el("div", { class: "table-scroll" }, table),
    drawer);
}

function cellDrawer(ab: string, tier: string, c: CellReport): HTMLElement {
  const t = el("table", { class: "grid" },
    el("tr", null,
      el("th", null, "query"), el("th", null, "question"),
      el("th", null, "r@5"), el("th", null, "r@∞"), el("th", null, "mrr"),
      el("th", null, "grounded"), el("th", null, "diagnosis")));
  for (const q of c.queries) {
    t.append(el("tr", null,
      el("td", null, q.id),
      el("td", { class: "q" }, q.question),
      el("td", { class: "num" }, f3(q.r5)),
      el("td", { class: "num" }, f3(q.rinf)),
      el("td", { class: "num" }, f3(q.mrr)),
      el("td", null, el("span", { class: "dot " + (q.grounded ? "good" : "bad") }),
        " ", q.grounded ? "yes" : "no"),
      el("td", { class: "q" }, q.note)));
  }
  return el("div", null,
    el("div", { class: "subhead" }, `${ab} × ${tier} — ${c.n} queries, ${c.n_targets} targets`),
    metricStrip(c),
    el("div", { class: "table-scroll" }, t));
}

function metricStrip(c: CellReport): HTMLElement {
  const pairs: [string, string][] = [
    ["r@1 strict/loose", `${f3(c.r1_strict)} / ${f3(c.r1_loose)}`],
    ["r@5 strict/loose", `${f3(c.r5_strict)} / ${f3(c.r5_loose)}`],
    ["r@∞ strict/loose", `${f3(c.rinf_strict)} / ${f3(c.rinf_loose)}`],
    ["mrr", f3(c.mrr)],
    ["grounded", pct(c.grounded) + "%"],
    ["mention F2 strict μ/M", `${f3(c.mention_f_strict_micro)} / ${f3(c.mention_f_strict_macro)}`],
    ["mention F2 weak μ/M", `${f3(c.mention_f_weak_micro)} / ${f3(c.mention_f_weak_macro)}`],
  ];
  const strip = el("div", null);
  for (const [k, v] of pairs) {
    strip.append(el("span", { class: "chip", style: "margin: 0 6px 6px 0;" }, `${k}: ${v}`));
  }
  return strip;
}

function nilSection(run: RunFile): HTMLElement {
  const t = el("table", { class: "grid" },
    el("tr", null,
      el("th", null, "ablation"), el("th", null, "NIL precision"),
      el("th", null, "NIL recall"), el("th", null, "confident-wrong")));
  const wrongs: HTMLElement[] = [];
  for (const ab of run.ablations) {
    const n = run.nil[ab];
    if (!n) continue;
    t.append(el("tr", null,
      el("td", null, ab),
      el("td", { class: "num" }, n.precision === null ? "—" : f3(n.precision)),
      el("td", { class: "num" }, n.recall === null ? "—" : f3(n.recall)),
      el("td", { class: "num" }, String(n.confident_wrong.length))));
    for (const w of n.confident_wrong) {
      wrongs.push(el("div", { class: "fail-item" },
        el("div", { class: "fail-title" },
          el("span", { class: "dot bad" }), `${ab} — ${w.id}`),
        el("div", { class: "fail-detail" }, w.question),
        el("div", { class: "fail-queries" }, `resolved to ${w.candidate} (score ${f3(w.score)})`)));
    }
  }
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "honest absence (NIL)"),
    el("div", { class: "lede" },
      "precision: correct absences over all absence outcomes. recall: NIL queries that did not ",
      "produce a confident wrong mention. every confident-wrong is a named case, not just a rate."),
    el("div", { class: "table-scroll" }, t),
    wrongs.length ? el("div", { class: "panel" }, ...wrongs)
                  : el("div", { class: "empty" }, "no confident-wrong cases in this run"));
}

function calibrationSection(run: RunFile): HTMLElement {
  const grid = el("div", { class: "calgrid" });
  for (const ab of run.ablations) {
    const buckets = run.calibration[ab];
    if (!buckets) continue;
    grid.append(el("div", { class: "calcard panel" },
      el("div", { class: "subhead" }, ab),
      calibrationSvg(buckets),
      el("div", { class: "callabel" },
        "P(gold | score bucket) — dashed identity is perfect calibration")));
  }
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "calibration"),
    el("div", { class: "lede" },
      "the fused scores claim absolute meaning (bands at 0.35 / 0.85 / 0.9); this curve is the ",
      "direct test. bucket area scales with sample count."),
    grid);
}

function calibrationSvg(buckets: CalibrationBucket[]): SVGElement {
  const W = 260, H = 180, L = 34, B = 24, T = 8, R = 8;
  const pw = W - L - R, ph = H - T - B;
  const x = (v: number) => L + v * pw;
  const y = (v: number) => T + (1 - v) * ph;
  const svg = svgEl("svg", { viewBox: `0 0 ${W} ${H}`, role: "img" });
  // hairline frame + gridlines
  for (const v of [0, 0.5, 1]) {
    svg.append(svgEl("line", {
      x1: x(0), y1: y(v), x2: x(1), y2: y(v),
      stroke: "var(--rule)", "stroke-width": 1 }));
    svg.append(svgEl("text", {
      x: L - 5, y: y(v) + 3, "text-anchor": "end",
      "font-size": 8, fill: "var(--ink-faint)", "font-family": "var(--mono)" }, f3(v)));
  }
  for (const v of [0, 0.5, 1]) {
    svg.append(svgEl("text", {
      x: x(v), y: H - 8, "text-anchor": "middle",
      "font-size": 8, fill: "var(--ink-faint)", "font-family": "var(--mono)" }, String(v)));
  }
  // identity reference
  svg.append(svgEl("line", {
    x1: x(0), y1: y(0), x2: x(1), y2: y(1),
    stroke: "var(--flat)", "stroke-width": 1, "stroke-dasharray": "3 3" }));
  // curve over non-empty buckets
  const pts = buckets.filter((b) => b.n > 0);
  if (pts.length > 0) {
    const path = pts.map((b, i) =>
      `${i === 0 ? "M" : "L"}${x((b.lo + b.hi) / 2).toFixed(1)},${y(b.p_gold).toFixed(1)}`).join(" ");
    svg.append(svgEl("path", { d: path, fill: "none", stroke: "var(--accent)", "stroke-width": 1.5 }));
    const maxN = Math.max(...pts.map((b) => b.n));
    for (const b of pts) {
      svg.append(svgEl("circle", {
        cx: x((b.lo + b.hi) / 2), cy: y(b.p_gold),
        r: 1.5 + 3.5 * Math.sqrt(b.n / maxN),
        fill: "var(--accent)", "fill-opacity": 0.75 }));
    }
  } else {
    svg.append(svgEl("text", {
      x: x(0.5), y: y(0.5), "text-anchor": "middle",
      "font-size": 9, fill: "var(--ink-faint)", "font-family": "var(--mono)" }, "no selected candidates"));
  }
  return svg;
}

function costSection(run: RunFile): HTMLElement {
  const t = el("table", { class: "grid" },
    el("tr", null,
      el("th", null, "ablation"), el("th", null, "tier"),
      el("th", null, "median ms"), el("th", null, "p95 ms"),
      el("th", null, "dense probes/q"), el("th", null, "adjudication rate"),
      el("th", null, "selected/mention")));
  for (const ab of run.ablations) {
    for (const tier of run.tiers) {
      const c = run.cells[ab]?.[tier];
      if (!c) continue;
      t.append(el("tr", null,
        el("td", null, ab), el("td", null, tier),
        el("td", { class: "num" }, c.latency_median_ms.toFixed(1)),
        el("td", { class: "num" }, c.latency_p95_ms.toFixed(1)),
        el("td", { class: "num" }, c.dense_probes_mean.toFixed(2)),
        el("td", { class: "num" }, f3(c.adjudication_rate)),
        el("td", { class: "num" }, c.selected_per_mention.toFixed(2))));
    }
  }
  const bt = el("table", { class: "grid" },
    el("tr", null,
      el("th", null, "ablation"), el("th", null, "embed calls"),
      el("th", null, "texts embedded"), el("th", null, "embed ms total"),
      el("th", null, "lm calls"), el("th", null, "lm ms mean")));
  for (const ab of run.ablations) {
    const b = run.backend_cost[ab];
    if (!b) continue;
    bt.append(el("tr", null,
      el("td", null, ab),
      el("td", { class: "num" }, String(b.embed_calls)),
      el("td", { class: "num" }, String(b.embed_texts)),
      el("td", { class: "num" }, b.embed_ms_total.toFixed(0)),
      el("td", { class: "num" }, String(b.lm_calls)),
      el("td", { class: "num" }, b.lm_ms_mean.toFixed(0))));
  }
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "cost"),
    el("div", { class: "lede" },
      "every mechanism's lift is quoted with its cost or not at all: latency next to recall, ",
      "probe counts and LM routing measured at the backend seams."),
    el("div", { class: "table-scroll" }, t),
    el("div", { class: "subhead" }, "backend round-trips"),
    el("div", { class: "table-scroll" }, bt));
}

function tukeySection(run: RunFile): HTMLElement | null {
  const tiers = Object.keys(run.tukey);
  if (tiers.length === 0) return null;
  const t = el("table", { class: "grid" },
    el("tr", null,
      el("th", null, "tier"), el("th", null, "pair"),
      el("th", null, "adjusted p"), el("th", null, "verdict")));
  for (const tier of tiers) {
    for (const p of run.tukey[tier]) {
      t.append(el("tr", null,
        el("td", null, tier),
        el("td", null, `${p.a} vs ${p.b}`),
        el("td", { class: "num" }, p.p.toFixed(4)),
        el("td", null, p.p < 0.05
          ? el("span", { class: "pill caution" }, "significant")
          : el("span", { class: "faint" }, "n.s."))));
    }
  }
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "multiple comparisons — randomised Tukey HSD"),
    el("div", { class: "lede" },
      "familywise-adjusted pairwise differences across the ablation sweep (per-query recall@5)."),
    el("div", { class: "table-scroll" }, t));
}

function failuresSection(run: RunFile): HTMLElement {
  const body = run.failures.length === 0
    ? el("div", { class: "empty" },
        run.pass === null ? "ungraded: no accepted baseline for this corpus yet"
                          : "all grading checks passed")
    : el("div", { class: "panel" }, run.failures.map((f) =>
        el("div", { class: "fail-item" },
          el("div", { class: "fail-title" },
            el("span", { class: "dot bad" }),
            el("span", { class: "chip" }, f.check), " ", f.cell),
          el("div", { class: "fail-detail" }, f.detail),
          f.queries.length ? el("div", { class: "fail-queries" }, f.queries.join("  ")) : null)));
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "named failures"),
    body);
}

function notesSection(run: RunFile): HTMLElement | null {
  if (run.notes.length === 0) return null;
  return el("div", { class: "section" },
    el("div", { class: "h2" }, "run notes"),
    el("div", { class: "panel mono", style: "font-size: 11.5px; line-height: 1.7;" },
      run.notes.map((n) => el("div", null, n))));
}

/* ---------- boot ---------- */

function main(): void {
  const blob = document.getElementById("run-data");
  if (!blob) return;
  const run: RunFile = JSON.parse(blob.textContent ?? "{}");

  buildThemePicker();
  buildTypePicker();
  document.addEventListener("click", () => closeAllMenus());

  document.title = `stemma eval — ${run.run_id}`;
  const crumbs = document.getElementById("crumbs");
  if (crumbs) crumbs.textContent = `eval / ${run.corpus} / ${run.run_id}`;

  const page = document.getElementById("page") as HTMLElement;
  const sections: (HTMLElement | null)[] = [
    header(run),
    matrixSection(run),
    nilSection(run),
    calibrationSection(run),
    costSection(run),
    tukeySection(run),
    failuresSection(run),
    notesSection(run),
  ];
  for (const s of sections) if (s) page.append(s);
}

main();
