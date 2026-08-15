// report.ts
function el(tag, attrs, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === null || v === void 0) continue;
    if (k === "class") n.className = String(v);
    else if (k.startsWith("on") && typeof v === "function") {
      n.addEventListener(k.slice(2), v);
    } else n.setAttribute(k, String(v));
  }
  appendChildren(n, children);
  return n;
}
function svgEl(tag, attrs, ...children) {
  const n = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs ?? {})) n.setAttribute(k, String(v));
  appendChildren(n, children);
  return n;
}
function appendChildren(n, children) {
  for (const c of children) {
    if (c === null || c === void 0) continue;
    if (Array.isArray(c)) appendChildren(n, c);
    else n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}
var COLOR_THEMES = [
  [
    "monokai",
    "monokai",
    [
      "#1e1f1c",
      "#272822",
      "#f8f8f2",
      "#a6e22e",
      "#f92672",
      "#66d9ef"
    ]
  ],
  [
    "solarized-dark",
    "solarized dark",
    [
      "#04222B",
      "#0A2D38",
      "#93A1A1",
      "#8BB80E",
      "#E0483C",
      "#2AA198"
    ]
  ],
  [
    "solarized-light",
    "solarized light",
    [
      "#FDF6E3",
      "#FBF1D6",
      "#586E75",
      "#6B9B0B",
      "#DC322F",
      "#268BD2"
    ]
  ],
  [
    "google-light",
    "google light",
    [
      "#FFFFFF",
      "#F4F4F4",
      "#474A4E",
      "#34A853",
      "#EA4335",
      "#1B9CB8"
    ]
  ],
  [
    "google-dark",
    "google dark",
    [
      "#202124",
      "#2C2D30",
      "#FFFFFF",
      "#34A853",
      "#EA4335",
      "#24C1E0"
    ]
  ],
  [
    "lunaria-light",
    "lunaria light",
    [
      "#EBE4E1",
      "#E2DCD9",
      "#363434",
      "#497D46",
      "#783C1F",
      "#3778A9"
    ]
  ],
  [
    "lunaria-eclipse",
    "lunaria eclipse",
    [
      "#323F46",
      "#3B484F",
      "#DFE2ED",
      "#BEDBC1",
      "#BA9088",
      "#C8429F"
    ]
  ],
  [
    "belafonte-day",
    "belafonte day",
    [
      "#D5CCBA",
      "#CCC3B2",
      "#34292D",
      "#6E6A4E",
      "#BE100E",
      "#426A79"
    ]
  ],
  [
    "belafonte-night",
    "belafonte night",
    [
      "#20111B",
      "#271821",
      "#D5CCBA",
      "#A6A07A",
      "#D6403E",
      "#6F8E97"
    ]
  ],
  [
    "paper",
    "paper",
    [
      "#F2EEDE",
      "#E6E2D3",
      "#1A1A1A",
      "#216609",
      "#CC3E28",
      "#1E6FCC"
    ]
  ],
  [
    "zenburn",
    "zenburn",
    [
      "#3A3A3A",
      "#424241",
      "#DCDCCC",
      "#8FB28F",
      "#CC9393",
      "#8CD0D3"
    ]
  ],
  [
    "selenized-black",
    "selenized black",
    [
      "#181818",
      "#202020",
      "#DEDEDE",
      "#83C746",
      "#FF5E56",
      "#56D8C9"
    ]
  ],
  [
    "relaxed",
    "relaxed",
    [
      "#353A44",
      "#3D424B",
      "#F7F7F7",
      "#A0AC77",
      "#BC5653",
      "#7EAAC7"
    ]
  ],
  [
    "espresso",
    "espresso",
    [
      "#323232",
      "#3A3A3A",
      "#FFFFFF",
      "#A5C261",
      "#D25252",
      "#6C99BB"
    ]
  ],
  [
    "dracula",
    "dracula",
    [
      "#282A36",
      "#343746",
      "#F8F8F2",
      "#50FA7B",
      "#FF5555",
      "#BD93F9"
    ]
  ],
  [
    "ubuntu",
    "ubuntu",
    [
      "#300A24",
      "#3D1530",
      "#EEEEEC",
      "#8AE234",
      "#CC0000",
      "#34E2E2"
    ]
  ]
];
var MONO_GSM = '"Google Sans Mono","Noto Sans Mono",ui-monospace,monospace';
var SANS_GROTESK = '"Space Grotesk",system-ui,sans-serif';
var TYPE_OPTIONS = [
  {
    id: "T7",
    label: "Google Sans Mono",
    group: "technical",
    head: MONO_GSM,
    body: MONO_GSM
  },
  {
    id: "T9",
    label: "Source Sans 3 + Source Code Pro",
    group: "technical",
    head: '"Source Sans 3",system-ui,sans-serif',
    body: '"Source Sans 3",system-ui,sans-serif'
  },
  {
    id: "T12",
    label: "Inconsolata",
    group: "technical",
    head: '"Inconsolata",ui-monospace,monospace',
    body: '"Inconsolata",ui-monospace,monospace'
  },
  {
    id: "T14",
    label: "Ubuntu + Ubuntu Mono",
    group: "technical",
    head: '"Ubuntu",system-ui,sans-serif',
    body: '"Ubuntu",system-ui,sans-serif'
  },
  {
    id: "E5",
    label: "Fraunces",
    group: "editorial",
    head: '"Fraunces",Georgia,serif',
    body: '"Fraunces",Georgia,serif'
  },
  {
    id: "E7",
    label: "Bitter",
    group: "editorial",
    head: '"Bitter",Georgia,serif',
    body: '"Bitter",Georgia,serif'
  },
  {
    id: "E8",
    label: "Literata",
    group: "editorial",
    head: '"Literata",Georgia,serif',
    body: '"Literata",Georgia,serif'
  },
  {
    id: "E15",
    label: "Domine",
    group: "editorial",
    head: '"Domine",Georgia,serif',
    body: '"Domine",Georgia,serif'
  },
  {
    id: "D2",
    label: "Archivo Narrow + Space Grotesk",
    group: "display",
    head: `"Archivo Narrow",${SANS_GROTESK}`,
    body: SANS_GROTESK
  },
  {
    id: "D12",
    label: "Hanken Grotesk",
    group: "display",
    head: '"Hanken Grotesk",system-ui,sans-serif',
    body: '"Hanken Grotesk",system-ui,sans-serif'
  },
  {
    id: "D14",
    label: "Barlow Condensed + Space Grotesk",
    group: "display",
    head: '"Barlow Condensed","Archivo Narrow",system-ui,sans-serif',
    body: SANS_GROTESK
  },
  {
    id: "D5",
    label: "Bricolage Grotesque",
    group: "display",
    head: '"Bricolage Grotesque",system-ui,sans-serif',
    body: '"Bricolage Grotesque",system-ui,sans-serif'
  }
];
var SAMPLE_LINE = "the quick brown fox 0123";
var FONT_SIZES = [
  [
    "s",
    1
  ],
  [
    "m",
    1.15
  ],
  [
    "l",
    1.3
  ]
];
var openMenus = [];
function closeAllMenus(except) {
  for (const m of openMenus) {
    if (m !== except) m.classList.remove("open");
  }
}
function swatchStrip(colors) {
  const strip = el("span", {
    class: "swatch-strip",
    "aria-hidden": "true"
  });
  for (const c of colors) {
    const chip = el("i");
    chip.style.background = c;
    strip.append(chip);
  }
  return strip;
}
function store(key) {
  try {
    return store(key);
  } catch {
    return null;
  }
}
function storeSet(key, v) {
  try {
    storeSet(key, v);
  } catch {
  }
}
function buildThemePicker() {
  const mount = document.getElementById("themepicker");
  const saved = store("stemma.theme") ?? "paper";
  document.documentElement.dataset.theme = saved;
  const current = COLOR_THEMES.find((t) => t[0] === saved) ?? COLOR_THEMES[9];
  const trigName = el("span", {
    class: "cd-name"
  }, current[1]);
  const trigStrip = el("span", null, swatchStrip(current[2]));
  const list = el("div", {
    class: "cd-list",
    role: "listbox"
  });
  const cd = el("span", {
    class: "cd"
  }, el("button", {
    class: "cd-trigger",
    onclick: (e) => {
      e.stopPropagation();
      closeAllMenus(cd);
      cd.classList.toggle("open");
    }
  }, trigStrip, trigName, el("span", {
    class: "cd-caret"
  }, "\u25BE")), list);
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
      }
    }, swatchStrip(colors), el("span", {
      class: "cd-name"
    }, label));
    list.append(opt);
  }
  openMenus.push(cd);
  mount.append(cd);
}
function buildTypePicker() {
  const mount = document.getElementById("typepicker");
  const saved = store("stemma.type") ?? "T9";
  if (saved !== "T9") document.documentElement.dataset.type = saved;
  const savedSize = store("stemma.fontsize") ?? "s";
  const sizeVal = FONT_SIZES.find(([k]) => k === savedSize)?.[1] ?? 1;
  if (sizeVal !== 1) document.documentElement.style.setProperty("--fs", String(sizeVal));
  const current = TYPE_OPTIONS.find((o) => o.id === saved) ?? TYPE_OPTIONS[1];
  const trigName = el("span", {
    class: "cd-name"
  }, current.label);
  const trigSpec = el("span", {
    class: "tf-spec"
  }, "Ag");
  trigSpec.style.fontFamily = current.head;
  const listBox = el("div", {
    class: "tf-list"
  });
  const pop = el("div", {
    class: "tf-pop"
  }, listBox);
  const cd = el("span", {
    class: "cd"
  }, el("button", {
    class: "cd-trigger",
    onclick: (e) => {
      e.stopPropagation();
      closeAllMenus(cd);
      cd.classList.toggle("open");
    }
  }, trigSpec, trigName, el("span", {
    class: "cd-caret"
  }, "\u25BE")), pop);
  for (const group of [
    "technical",
    "editorial",
    "display"
  ]) {
    listBox.append(el("div", {
      class: "tf-group"
    }, el("div", {
      class: "subhead"
    }, group)));
    for (const o of TYPE_OPTIONS.filter((x) => x.group === group)) {
      const name = el("span", {
        class: "tf-name"
      }, o.label);
      name.style.fontFamily = o.head;
      const sample = el("span", {
        class: "tf-sample",
        "aria-hidden": "true"
      }, SAMPLE_LINE);
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
        }
      }, name, sample);
      listBox.append(opt);
    }
  }
  const seg = el("span", {
    class: "seg"
  });
  for (const [k, v] of FONT_SIZES) {
    const b = el("button", {
      class: k === savedSize ? "on" : "",
      onclick: () => {
        document.documentElement.style.setProperty("--fs", String(v));
        storeSet("stemma.fontsize", k);
        seg.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
        b.classList.add("on");
      }
    }, k);
    seg.append(b);
  }
  pop.append(el("div", {
    class: "tf-foot"
  }, el("span", {
    class: "k"
  }, "text size"), seg));
  openMenus.push(cd);
  mount.append(cd);
}
var f3 = (x) => x.toFixed(3);
var pct = (x) => (100 * x).toFixed(1);
var signed = (x) => (x >= 0 ? "+" : "") + (100 * x).toFixed(1);
function header(run) {
  const pass = run.pass === null ? el("span", {
    class: "pill neutral"
  }, "ungraded \u2014 no baseline") : run.pass ? el("span", {
    class: "pill good"
  }, el("span", {
    class: "dot good"
  }), "pass") : el("span", {
    class: "pill bad"
  }, el("span", {
    class: "dot bad"
  }), "fail");
  return el("div", {
    class: "section"
  }, el("div", {
    class: "runhead"
  }, el("span", {
    class: "h1"
  }, "evaluation run \u2014 ", run.corpus), pass), el("div", {
    class: "kv"
  }, el("span", {
    class: "k"
  }, "run id"), el("span", {
    class: "v"
  }, run.run_id), el("span", {
    class: "k"
  }, "git rev"), el("span", {
    class: "v"
  }, run.git_rev), el("span", {
    class: "k"
  }, "date (utc)"), el("span", {
    class: "v"
  }, run.date), el("span", {
    class: "k"
  }, "dataset"), el("span", {
    class: "v"
  }, run.dataset), el("span", {
    class: "k"
  }, "ablations"), el("span", {
    class: "v"
  }, run.ablations.join(" \u2192 "))));
}
function cellDelta(c) {
  return c.delta_baseline ?? c.delta_prev;
}
function matrixSection(run) {
  const drawer = el("div", {
    class: "drawer"
  });
  let openKey = "";
  const table = el("table", {
    class: "grid matrix"
  });
  const head = el("tr", null, el("th", null, "mechanism"));
  for (const tier of run.tiers) head.append(el("th", null, tier));
  table.append(head);
  for (const ab of run.ablations) {
    const row = el("tr", null, el("td", {
      class: "mono"
    }, ab));
    for (const tier of run.tiers) {
      const c = run.cells[ab]?.[tier];
      if (!c) {
        row.append(el("td", null, el("div", {
          class: "cellbox",
          disabled: "true"
        }, el("span", {
          class: "cell-r5 faint"
        }, "\u2014"))));
        continue;
      }
      const d = cellDelta(c);
      const key = `${ab}\xD7${tier}`;
      const box = el("button", {
        class: "cellbox",
        onclick: () => {
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
        }
      }, el("span", {
        class: "cell-r5"
      }, f3(c.r5_strict)), d ? el("span", {
        class: "cell-delta " + (d.mean > 5e-4 ? "up" : d.mean < -5e-4 ? "down" : "")
      }, `${signed(d.mean)} (${d.vs.replace("prev:", "vs ")})`) : el("span", {
        class: "cell-delta"
      }, "n=" + c.n), d ? el("span", {
        class: "cell-ci"
      }, `ci [${signed(d.ci[0])}, ${signed(d.ci[1])}] p=${d.p.toFixed(3)}`) : null);
      row.append(el("td", null, box));
    }
    table.append(row);
  }
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "mechanism \xD7 tier matrix"), el("div", {
    class: "lede"
  }, "column-strict recall@5 per cell; deltas vs the accepted baseline where one exists, ", "else vs the previous ablation. click a cell for its per-query list."), el("div", {
    class: "table-scroll"
  }, table), drawer);
}
function cellDrawer(ab, tier, c) {
  const t = el("table", {
    class: "grid"
  }, el("tr", null, el("th", null, "query"), el("th", null, "question"), el("th", null, "r@5"), el("th", null, "r@\u221E"), el("th", null, "mrr"), el("th", null, "grounded"), el("th", null, "diagnosis")));
  for (const q of c.queries) {
    t.append(el("tr", null, el("td", null, q.id), el("td", {
      class: "q"
    }, q.question), el("td", {
      class: "num"
    }, f3(q.r5)), el("td", {
      class: "num"
    }, f3(q.rinf)), el("td", {
      class: "num"
    }, f3(q.mrr)), el("td", null, el("span", {
      class: "dot " + (q.grounded ? "good" : "bad")
    }), " ", q.grounded ? "yes" : "no"), el("td", {
      class: "q"
    }, q.note)));
  }
  return el("div", null, el("div", {
    class: "subhead"
  }, `${ab} \xD7 ${tier} \u2014 ${c.n} queries, ${c.n_targets} targets`), metricStrip(c), el("div", {
    class: "table-scroll"
  }, t));
}
function metricStrip(c) {
  const pairs = [
    [
      "r@1 strict/loose",
      `${f3(c.r1_strict)} / ${f3(c.r1_loose)}`
    ],
    [
      "r@5 strict/loose",
      `${f3(c.r5_strict)} / ${f3(c.r5_loose)}`
    ],
    [
      "r@\u221E strict/loose",
      `${f3(c.rinf_strict)} / ${f3(c.rinf_loose)}`
    ],
    [
      "mrr",
      f3(c.mrr)
    ],
    [
      "grounded",
      pct(c.grounded) + "%"
    ],
    [
      "mention F2 strict \u03BC/M",
      `${f3(c.mention_f_strict_micro)} / ${f3(c.mention_f_strict_macro)}`
    ],
    [
      "mention F2 weak \u03BC/M",
      `${f3(c.mention_f_weak_micro)} / ${f3(c.mention_f_weak_macro)}`
    ]
  ];
  const strip = el("div", null);
  for (const [k, v] of pairs) {
    strip.append(el("span", {
      class: "chip",
      style: "margin: 0 6px 6px 0;"
    }, `${k}: ${v}`));
  }
  return strip;
}
function nilSection(run) {
  const t = el("table", {
    class: "grid"
  }, el("tr", null, el("th", null, "ablation"), el("th", null, "NIL precision"), el("th", null, "NIL recall"), el("th", null, "confident-wrong")));
  const wrongs = [];
  for (const ab of run.ablations) {
    const n = run.nil[ab];
    if (!n) continue;
    t.append(el("tr", null, el("td", null, ab), el("td", {
      class: "num"
    }, n.precision === null ? "\u2014" : f3(n.precision)), el("td", {
      class: "num"
    }, n.recall === null ? "\u2014" : f3(n.recall)), el("td", {
      class: "num"
    }, String(n.confident_wrong.length))));
    for (const w of n.confident_wrong) {
      wrongs.push(el("div", {
        class: "fail-item"
      }, el("div", {
        class: "fail-title"
      }, el("span", {
        class: "dot bad"
      }), `${ab} \u2014 ${w.id}`), el("div", {
        class: "fail-detail"
      }, w.question), el("div", {
        class: "fail-queries"
      }, `resolved to ${w.candidate} (score ${f3(w.score)})`)));
    }
  }
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "honest absence (NIL)"), el("div", {
    class: "lede"
  }, "precision: correct absences over all absence outcomes. recall: NIL queries that did not ", "produce a confident wrong mention. every confident-wrong is a named case, not just a rate."), el("div", {
    class: "table-scroll"
  }, t), wrongs.length ? el("div", {
    class: "panel"
  }, ...wrongs) : el("div", {
    class: "empty"
  }, "no confident-wrong cases in this run"));
}
function calibrationSection(run) {
  const grid = el("div", {
    class: "calgrid"
  });
  for (const ab of run.ablations) {
    const buckets = run.calibration[ab];
    if (!buckets) continue;
    grid.append(el("div", {
      class: "calcard panel"
    }, el("div", {
      class: "subhead"
    }, ab), calibrationSvg(buckets), el("div", {
      class: "callabel"
    }, "observed gold-link rate within each fused-score bucket")));
  }
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "score-conditioned accuracy"), el("div", {
    class: "lede"
  }, "this descriptive view does not treat fused scores as probabilities. point area scales ", "with sample count."), grid);
}
function calibrationSvg(buckets) {
  const W = 260, H = 180, L = 34, B = 24, T = 8, R = 8;
  const pw = W - L - R, ph = H - T - B;
  const x = (v) => L + v * pw;
  const y = (v) => T + (1 - v) * ph;
  const svg = svgEl("svg", {
    viewBox: `0 0 ${W} ${H}`,
    role: "img"
  });
  for (const v of [
    0,
    0.5,
    1
  ]) {
    svg.append(svgEl("line", {
      x1: x(0),
      y1: y(v),
      x2: x(1),
      y2: y(v),
      stroke: "var(--rule)",
      "stroke-width": 1
    }));
    svg.append(svgEl("text", {
      x: L - 5,
      y: y(v) + 3,
      "text-anchor": "end",
      "font-size": 8,
      fill: "var(--ink-faint)",
      "font-family": "var(--mono)"
    }, f3(v)));
  }
  for (const v of [
    0,
    0.5,
    1
  ]) {
    svg.append(svgEl("text", {
      x: x(v),
      y: H - 8,
      "text-anchor": "middle",
      "font-size": 8,
      fill: "var(--ink-faint)",
      "font-family": "var(--mono)"
    }, String(v)));
  }
  const pts = buckets.filter((b) => b.n > 0);
  if (pts.length > 0) {
    const path = pts.map((b, i) => `${i === 0 ? "M" : "L"}${x((b.lo + b.hi) / 2).toFixed(1)},${y(b.p_gold).toFixed(1)}`).join(" ");
    svg.append(svgEl("path", {
      d: path,
      fill: "none",
      stroke: "var(--accent)",
      "stroke-width": 1.5
    }));
    const maxN = Math.max(...pts.map((b) => b.n));
    for (const b of pts) {
      svg.append(svgEl("circle", {
        cx: x((b.lo + b.hi) / 2),
        cy: y(b.p_gold),
        r: 1.5 + 3.5 * Math.sqrt(b.n / maxN),
        fill: "var(--accent)",
        "fill-opacity": 0.75
      }));
    }
  } else {
    svg.append(svgEl("text", {
      x: x(0.5),
      y: y(0.5),
      "text-anchor": "middle",
      "font-size": 9,
      fill: "var(--ink-faint)",
      "font-family": "var(--mono)"
    }, "no selected candidates"));
  }
  return svg;
}
function costSection(run) {
  const t = el("table", {
    class: "grid"
  }, el("tr", null, el("th", null, "ablation"), el("th", null, "tier"), el("th", null, "median ms"), el("th", null, "p95 ms"), el("th", null, "dense probes/q"), el("th", null, "adjudication rate"), el("th", null, "selected/mention")));
  for (const ab of run.ablations) {
    for (const tier of run.tiers) {
      const c = run.cells[ab]?.[tier];
      if (!c) continue;
      t.append(el("tr", null, el("td", null, ab), el("td", null, tier), el("td", {
        class: "num"
      }, c.latency_median_ms.toFixed(1)), el("td", {
        class: "num"
      }, c.latency_p95_ms.toFixed(1)), el("td", {
        class: "num"
      }, c.dense_probes_mean.toFixed(2)), el("td", {
        class: "num"
      }, f3(c.adjudication_rate)), el("td", {
        class: "num"
      }, c.selected_per_mention.toFixed(2))));
    }
  }
  const bt = el("table", {
    class: "grid"
  }, el("tr", null, el("th", null, "ablation"), el("th", null, "embed calls"), el("th", null, "texts embedded"), el("th", null, "embed ms total"), el("th", null, "lm calls"), el("th", null, "lm ms mean")));
  for (const ab of run.ablations) {
    const b = run.backend_cost[ab];
    if (!b) continue;
    bt.append(el("tr", null, el("td", null, ab), el("td", {
      class: "num"
    }, String(b.embed_calls)), el("td", {
      class: "num"
    }, String(b.embed_texts)), el("td", {
      class: "num"
    }, b.embed_ms_total.toFixed(0)), el("td", {
      class: "num"
    }, String(b.lm_calls)), el("td", {
      class: "num"
    }, b.lm_ms_mean.toFixed(0))));
  }
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "cost"), el("div", {
    class: "lede"
  }, "every mechanism's lift is quoted with its cost or not at all: latency next to recall, ", "probe counts and LM routing measured at the backend seams."), el("div", {
    class: "table-scroll"
  }, t), el("div", {
    class: "subhead"
  }, "backend round-trips"), el("div", {
    class: "table-scroll"
  }, bt));
}
function tukeySection(run) {
  const tiers = Object.keys(run.tukey);
  if (tiers.length === 0) return null;
  const t = el("table", {
    class: "grid"
  }, el("tr", null, el("th", null, "tier"), el("th", null, "pair"), el("th", null, "adjusted p"), el("th", null, "verdict")));
  for (const tier of tiers) {
    for (const p of run.tukey[tier]) {
      t.append(el("tr", null, el("td", null, tier), el("td", null, `${p.a} vs ${p.b}`), el("td", {
        class: "num"
      }, p.p.toFixed(4)), el("td", null, p.p < 0.05 ? el("span", {
        class: "pill caution"
      }, "significant") : el("span", {
        class: "faint"
      }, "n.s."))));
    }
  }
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "multiple comparisons \u2014 randomised Tukey HSD"), el("div", {
    class: "lede"
  }, "familywise-adjusted pairwise differences across the ablation sweep (per-query recall@5)."), el("div", {
    class: "table-scroll"
  }, t));
}
function failuresSection(run) {
  const body = run.failures.length === 0 ? el("div", {
    class: "empty"
  }, run.pass === null ? "ungraded: no accepted baseline for this corpus yet" : "all grading checks passed") : el("div", {
    class: "panel"
  }, run.failures.map((f) => el("div", {
    class: "fail-item"
  }, el("div", {
    class: "fail-title"
  }, el("span", {
    class: "dot bad"
  }), el("span", {
    class: "chip"
  }, f.check), " ", f.cell), el("div", {
    class: "fail-detail"
  }, f.detail), f.queries.length ? el("div", {
    class: "fail-queries"
  }, f.queries.join("  ")) : null)));
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "named failures"), body);
}
function notesSection(run) {
  if (run.notes.length === 0) return null;
  return el("div", {
    class: "section"
  }, el("div", {
    class: "h2"
  }, "run notes"), el("div", {
    class: "panel mono",
    style: "font-size: 11.5px; line-height: 1.7;"
  }, run.notes.map((n) => el("div", null, n))));
}
function main() {
  const blob = document.getElementById("run-data");
  if (!blob) return;
  const run = JSON.parse(blob.textContent ?? "{}");
  buildThemePicker();
  buildTypePicker();
  document.addEventListener("click", () => closeAllMenus());
  document.title = `stemma eval \u2014 ${run.run_id}`;
  const crumbs = document.getElementById("crumbs");
  if (crumbs) crumbs.textContent = `eval / ${run.corpus} / ${run.run_id}`;
  const page = document.getElementById("page");
  const sections = [
    header(run),
    matrixSection(run),
    nilSection(run),
    calibrationSection(run),
    costSection(run),
    tukeySection(run),
    failuresSection(run),
    notesSection(run)
  ];
  for (const s of sections) if (s) page.append(s);
}
main();
