// src/ui.ts
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
async function getJSON(url) {
  const r = await fetch(url);
  if (!r.ok) {
    let detail = r.statusText;
    try {
      detail = (await r.json()).detail ?? detail;
    } catch (_e) {
    }
    throw new Error(detail);
  }
  return r.json();
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;"
  })[c]);
}
function snippetNode(snippet) {
  const out = el("span", {
    class: "snippet"
  });
  const parts = snippet.split(/⟨([^⟩]*)⟩/);
  parts.forEach((p, i) => {
    if (i % 2 === 1) out.append(el("span", {
      class: "hit"
    }, p));
    else if (p) out.append(p);
  });
  return out;
}
var hovercard = document.getElementById("hovercard");
function hideHover() {
  hovercard.classList.remove("on");
}
function hov(node, html) {
  node.addEventListener("mouseenter", () => {
    hovercard.innerHTML = html;
    hovercard.classList.add("on");
  });
  node.addEventListener("mousemove", (ev) => {
    const e = ev;
    const pad = 14;
    let x = e.clientX + pad, y = e.clientY + pad;
    const r = hovercard.getBoundingClientRect();
    if (x + r.width > innerWidth - 8) x = e.clientX - r.width - pad;
    if (y + r.height > innerHeight - 8) y = e.clientY - r.height - pad;
    hovercard.style.left = `${x}px`;
    hovercard.style.top = `${y}px`;
  });
  node.addEventListener("mouseleave", () => hovercard.classList.remove("on"));
}
var state = {
  cfg: null,
  dbs: [],
  db: null,
  schema: null,
  view: "query"
};
var chatLog = /* @__PURE__ */ new Map();
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
function buildThemePicker() {
  const mount = document.getElementById("themepicker");
  const saved = localStorage.getItem("stemma.theme") ?? "paper";
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
      hideHover();
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
        localStorage.setItem("stemma.theme", id);
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
  const saved = localStorage.getItem("stemma.type") ?? "T9";
  if (saved !== "T9") document.documentElement.dataset.type = saved;
  const savedSize = localStorage.getItem("stemma.fontsize") ?? "s";
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
      hideHover();
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
          localStorage.setItem("stemma.type", o.id);
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
        localStorage.setItem("stemma.fontsize", k);
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
function initPickers() {
  buildThemePicker();
  buildTypePicker();
  document.addEventListener("click", () => closeAllMenus());
}
async function pollHealth() {
  const s = document.getElementById("status");
  const w = document.getElementById("statusword");
  try {
    const h = await getJSON("/api/health");
    s.className = "status " + (h.grpc ? "ok" : "down");
    w.textContent = h.grpc ? "live" : "grpc down";
  } catch (_e) {
    s.className = "status down";
    w.textContent = "ui down";
  }
  setTimeout(pollHealth, 8e3);
}
function renderSidebar() {
  const side = document.getElementById("sidebar");
  side.replaceChildren();
  if (state.dbs.length > 1) {
    side.append(el("div", {
      class: "tree-group"
    }, "database"));
    for (const d of state.dbs) {
      side.append(el("button", {
        class: "tree-node" + (d === state.db ? " sel" : ""),
        onclick: () => {
          state.db = d;
          state.schema = null;
          route();
          if (chatRailOpen()) renderChatRail();
        }
      }, el("span", {
        class: "tree-icon"
      }), d));
    }
  }
  side.append(el("div", {
    class: "tree-group"
  }, "tables"));
  if (!state.schema) {
    side.append(el("div", {
      class: "empty"
    }, "loading\u2026"));
    return;
  }
  const current = location.hash.startsWith("#/data/") ? decodeURIComponent(location.hash.slice(7).split("?")[0]) : null;
  for (const t of state.schema.tables) {
    side.append(el("button", {
      class: "tree-node" + (t.name === current ? " sel" : ""),
      onclick: () => {
        location.hash = "#/data/" + encodeURIComponent(t.name);
      }
    }, el("span", {
      class: "tree-icon round"
    }), t.name, el("span", {
      class: "count"
    }, "~" + t.row_count.toLocaleString())));
  }
  const storeBox = el("div", {
    class: "side-store"
  }, el("div", {
    class: "tree-group"
  }, "store"));
  side.append(storeBox);
  getJSON(`/api/db/${state.db}/store`).then((m) => {
    if (!m.exists) {
      storeBox.append(el("div", {
        class: "empty"
      }, "\u2014 not created yet"));
      return;
    }
    const pairs = [
      [
        "size",
        ((m.size_bytes ?? 0) / 1e6).toFixed(1) + " mb"
      ],
      [
        "lexical",
        m.lexical ? m.lexical.values.toLocaleString() + " values" : "\u2014"
      ],
      [
        "kg",
        m.kg ? `${m.kg.nodes} nodes \xB7 ${m.kg.edges} edges` : "\u2014"
      ],
      [
        "embed queue",
        String(m.embed_queue ?? 0)
      ],
      [
        "vectors",
        (m.model_registry ?? []).length ? `${(m.model_registry ?? []).length} tables` : "none \xB7 m3"
      ]
    ];
    storeBox.append(el("div", {
      class: "kv"
    }, pairs.map(([k, v]) => [
      el("span", {
        class: "k"
      }, k),
      el("span", {
        class: "v"
      }, v)
    ])));
  }).catch(() => storeBox.append(el("div", {
    class: "empty"
  }, "\u2014 unavailable")));
}
function setCrumbs(...parts) {
  document.getElementById("crumbs").textContent = [
    state.db,
    ...parts
  ].filter(Boolean).join(" \xB7 ");
}
function viewQuery(host, params) {
  const dialect = params.get("d") === "sql" ? "sql" : "nl";
  const q = params.get("q") ?? "";
  setCrumbs("query", dialect === "sql" ? "sql" : "natural");
  const seg = el("span", {
    class: "seg"
  }, el("button", {
    class: dialect === "nl" ? "on" : "",
    onclick: () => {
      location.hash = "#/query?d=nl" + (q ? "&q=" + encodeURIComponent(q) : "");
    }
  }, "natural"), el("button", {
    class: dialect === "sql" ? "on" : "",
    onclick: () => {
      location.hash = "#/query?d=sql";
    }
  }, "sql"));
  host.append(el("div", {
    style: "display:flex; align-items:baseline; gap:14px"
  }, el("h1", {
    class: "h1"
  }, "query"), seg), el("p", {
    class: "lede"
  }, dialect === "nl" ? "the dialect is natural language: mentions resolve to records, and the trajectory shows every span considered, every channel fired, and every candidate \u2014 chosen and near-miss alike." : "the dialect is sql, read-only: main is the .stemmadb store, src is the user database. every query ships with its plan."));
  if (dialect === "nl") queryNatural(host, q);
  else querySql(host);
}
function queryNatural(host, q) {
  const input = el("input", {
    class: "input",
    value: q,
    placeholder: "ask about the data \u2014 mentions resolve to records\u2026",
    onkeydown: (e) => {
      if (e.key === "Enter") run(input.value);
    }
  });
  const out = el("div", null);
  const examplesRow = el("div", null);
  host.append(el("div", {
    class: "queryrow"
  }, input, el("button", {
    class: "btn accent",
    onclick: () => run(input.value)
  }, "resolve")), examplesRow, out);
  getJSON(`/api/db/${state.db}/examples`).then((r) => {
    for (const x of r.examples) {
      examplesRow.append(el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        }
      }, x));
    }
    if (r.examples.length) {
      examplesRow.prepend(el("span", {
        class: "sql-caption",
        style: "margin-right:8px"
      }, "from the kg:"));
    }
  }).catch(() => {
  });
  if (q) run(q);
  async function run(query) {
    if (!query.trim()) return;
    history.replaceState(null, "", "#/query?d=nl&q=" + encodeURIComponent(query));
    document.getElementById("topsearch").value = query;
    out.replaceChildren(el("div", {
      class: "empty"
    }, "resolving\u2026"));
    let trace;
    if (pendingTrace && pendingTrace.query === query) {
      trace = pendingTrace;
      pendingTrace = null;
      renderTrace(out, trace);
      return;
    }
    try {
      trace = await getJSON(`/api/db/${state.db}/resolve?q=` + encodeURIComponent(query));
    } catch (e) {
      out.replaceChildren(el("div", {
        class: "sql-error"
      }, "resolution failed \u2014 " + e.message));
      return;
    }
    renderTrace(out, trace);
  }
}
function querySql(host) {
  const box = el("textarea", {
    class: "input sqlbox mono",
    placeholder: "SELECT \u2026"
  }, "SELECT src_table, src_column, count(*) AS n\nFROM lex_values GROUP BY 1, 2 ORDER BY n DESC");
  const out = el("div", null);
  const run = async () => {
    out.replaceChildren(el("div", {
      class: "empty"
    }, "running\u2026"));
    try {
      const r = await fetch(`/api/db/${state.db}/sql`, {
        method: "POST",
        headers: {
          "content-type": "application/json"
        },
        body: JSON.stringify({
          sql: box.value
        })
      });
      const d = await r.json();
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      out.replaceChildren(el("div", {
        class: "sql-caption"
      }, `${d.rows.length} row${d.rows.length === 1 ? "" : "s"}${d.truncated ? " (truncated)" : ""} \xB7 ${d.elapsed_ms} ms`), renderPlan(d.plan), el("div", {
        class: "table-scroll"
      }, el("table", {
        class: "grid"
      }, el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))), el("tbody", null, d.rows.map((row) => el("tr", null, row.map((v) => el("td", null, v === null ? "\u2205" : v))))))));
    } catch (e) {
      out.replaceChildren(el("div", {
        class: "sql-error"
      }, e.message));
    }
  };
  box.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") run();
  });
  host.append(box, el("div", {
    style: "margin-top:8px"
  }, el("button", {
    class: "btn accent",
    onclick: run
  }, "run"), el("span", {
    class: "sql-caption",
    style: "margin-left:10px"
  }, "ctrl-enter runs")), out);
}
function renderPlan(plan) {
  const box = el("div", {
    class: "plan panel"
  }, el("div", {
    class: "subhead"
  }, "query plan"));
  if (!plan.length) {
    box.append(el("div", {
      class: "empty"
    }, "\u2014 trivial plan"));
    return box;
  }
  for (const p of plan) {
    const opClass = /^SCAN/.test(p.detail) ? "scan" : /^SEARCH/.test(p.detail) ? "search" : "";
    box.append(el("div", {
      class: "plan-row"
    }, el("span", {
      class: "tick"
    }, "\u2502 ".repeat(p.depth) + "\u251C\u2500"), el("span", {
      class: `op ${opClass}`
    }, p.detail)));
  }
  return box;
}
function renderTrace(out, trace) {
  out.replaceChildren();
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);
  const qline = el("div", {
    class: "qline"
  });
  const covered = (pos) => mentionSpans.find((s) => pos >= s.start && pos < s.end);
  let cursor = 0;
  const tokenNodes = /* @__PURE__ */ new Map();
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    const m = covered(t.start);
    const cls = "qtok" + (m ? " mention" : t.stopword ? " stop" : "");
    const node = el("span", {
      class: cls
    }, t.text);
    if (m && !tokenNodes.has(m.id)) tokenNodes.set(m.id, node);
    qline.append(node);
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));
  const lanes = el("div", {
    class: "lanes"
  });
  const laneNodes = /* @__PURE__ */ new Map();
  for (const s of mentionSpans) {
    const lane = el("div", {
      class: "lane"
    }, el("div", {
      class: "lane-head"
    }, el("span", {
      class: "lane-span"
    }, s.text), el("span", {
      class: "lane-pos"
    }, `bytes ${s.start}\u2013${s.end}`), el("span", {
      class: "lane-pos"
    }, `${s.candidates.length} candidate${s.candidates.length === 1 ? "" : "s"}`)), s.candidates.map((c, i) => renderCandidate(c, i)));
    laneNodes.set(s.id, lane);
    lanes.append(lane);
  }
  if (!mentionSpans.length) {
    lanes.append(el("div", {
      class: "empty"
    }, "\u2014 no mentions resolved; every span is in the considered list below"));
  }
  const traj = el("div", {
    class: "traj"
  }, qline, lanes);
  const wires = svgEl("svg", {
    class: "wires",
    "aria-hidden": "true"
  });
  traj.prepend(wires);
  const also = trace.spans.filter((s) => s.status !== "selected" && s.status !== "skipped").sort((a, b) => a.start - b.start);
  const alsoBox = el("div", {
    class: "alsoran section"
  }, el("div", {
    class: "subhead"
  }, `spans considered \xB7 ${also.length}`));
  const statusPill = {
    overlapped: [
      "neutral",
      "overlapped"
    ],
    weak: [
      "caution",
      "weak"
    ],
    no_candidates: [
      "neutral",
      "no match"
    ]
  };
  for (const s of also) {
    const [tone, label] = statusPill[s.status] ?? [
      "neutral",
      s.status
    ];
    const row = el("div", {
      class: "alsoran-row"
    }, el("span", {
      class: "spantext"
    }, `\u201C${s.text}\u201D`), el("span", {
      class: "pill " + tone
    }, label), el("span", {
      class: "alsoran-cands"
    }, s.candidates.length ? s.candidates.map((c) => `${c.table}.${c.column} #${c.rowid} (${c.score.toFixed(2)})`).join(" \xB7 ") : "\u2014"));
    if (s.candidates.length) {
      hov(row, s.candidates.map((c) => `<b>${esc(c.table)}.${esc(c.column)}</b> #${c.rowid} \u201C${esc(c.snippet || c.value)}\u201D<br>score ${c.score.toFixed(3)} \xB7 ${esc(c.reject_reason)}`).join("<hr>"));
    }
    alsoBox.append(row);
  }
  if (!also.length) {
    alsoBox.append(el("div", {
      class: "empty"
    }, "\u2014 every considered span became a mention"));
  }
  out.append(el("div", {
    class: "sql-caption"
  }, `resolved in ${trace.elapsed_ms.toFixed(1)} ms \xB7 ${trace.spans.length} spans enumerated \xB7 channels: exact, bm25, trigram, kg`), traj, alsoBox);
  requestAnimationFrame(() => {
    const box = traj.getBoundingClientRect();
    wires.setAttribute("viewBox", `0 0 ${box.width} ${box.height}`);
    for (const s of mentionSpans) {
      const tok = tokenNodes.get(s.id), lane = laneNodes.get(s.id);
      if (!tok || !lane) continue;
      const a = tok.getBoundingClientRect(), b = lane.getBoundingClientRect();
      const x1 = a.left - box.left + a.width / 2, y1 = a.bottom - box.top + 2;
      const x2 = b.left - box.left + 22, y2 = b.top - box.top;
      wires.append(svgEl("path", {
        class: "wire",
        d: `M ${x1} ${y1} C ${x1} ${y1 + 28}, ${x2} ${y2 - 28}, ${x2} ${y2}`
      }));
    }
  });
}
function renderMiniTrace(trace) {
  const box = el("div", {
    class: "minitraj"
  });
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);
  const covered = (pos) => mentionSpans.some((s) => pos >= s.start && pos < s.end);
  const qline = el("div", {
    class: "mini-qline"
  });
  let cursor = 0;
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    qline.append(el("span", {
      class: "qtok" + (covered(t.start) ? " mention" : t.stopword ? " stop" : "")
    }, t.text));
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));
  box.append(qline);
  for (const sp of mentionSpans) {
    const sel = sp.candidates.filter((c) => c.selected);
    const missed = sp.candidates.length - sel.length;
    const lane = el("div", {
      class: "mini-lane"
    }, el("div", {
      class: "mini-span"
    }, sp.text));
    for (const c of sel.slice(0, 3)) {
      lane.append(el("div", {
        class: "mini-cand"
      }, el("span", {
        class: "mini-ref"
      }, `${c.table}.${c.column} #${c.rowid}`), c.is_doc && c.snippet ? snippetNode(c.snippet) : el("span", {
        class: "mini-val"
      }, `\u201C${c.value}\u201D`), el("span", {
        class: "meter"
      }, el("span", {
        style: `width:${Math.round(c.score * 100)}%`
      }))));
    }
    if (sel.length > 3 || missed > 0) {
      lane.append(el("div", {
        class: "mini-more"
      }, [
        sel.length > 3 ? `+${sel.length - 3} more` : "",
        missed > 0 ? `${missed} near-miss${missed === 1 ? "" : "es"}` : ""
      ].filter(Boolean).join(" \xB7 ")));
    }
    box.append(lane);
  }
  if (!mentionSpans.length) {
    box.append(el("div", {
      class: "empty"
    }, "\u2014 nothing resolved"));
  }
  return box;
}
function renderCandidate(c, rank) {
  const mid = c.is_doc && c.snippet ? snippetNode(c.snippet) : el("span", {
    class: "cand-val",
    title: c.value
  }, `\u201C${c.value}${c.value_truncated ? "\u2026" : ""}\u201D`);
  const row = el("div", {
    class: "cand " + (c.selected ? rank === 0 ? "sel-0" : "sel" : "rej")
  }, el("span", {
    class: "cand-id"
  }, `${c.table}.${c.column} `, el("span", {
    class: "rowid"
  }, `#${c.rowid}`)), mid, el("span", {
    class: "cand-right"
  }, el("span", {
    class: "cand-chips"
  }, c.channels.map((ch) => el("span", {
    class: "chip"
  }, ch.channel === "kg" ? `kg +${ch.raw}` : ch.channel)), c.selected ? null : el("span", {
    class: "pill bad"
  }, (c.reject_reason || "rejected").replace(/_/g, " "))), el("span", {
    style: "display:flex; gap:6px; align-items:center"
  }, el("span", {
    class: "meter"
  }, el("span", {
    style: `width:${Math.round(c.score * 100)}%`
  })), el("span", {
    class: "score"
  }, c.score.toFixed(2)))));
  hov(row, c.channels.map((ch) => ch.channel === "kg" ? `<b>kg</b> co-occurring terms matched: ${ch.raw}` : `<b>${esc(ch.channel)}</b> rank ${ch.rank + 1} \xB7 raw ${ch.raw.toFixed(3)}`).join("<br>") + (c.selected ? "" : `<hr>${esc(c.reject_reason.replace(/_/g, " "))}`));
  return row;
}
var pendingTrace = null;
function showTraceInMain(trace) {
  pendingTrace = trace;
  const target = "#/query?d=nl&q=" + encodeURIComponent(trace.query);
  if (location.hash === target) route();
  else location.hash = target;
}
function chatRailOpen() {
  return localStorage.getItem("stemma.chatrail") === "open";
}
function setChatRail(open) {
  localStorage.setItem("stemma.chatrail", open ? "open" : "closed");
  const rail = document.getElementById("chatrail");
  const grid = document.getElementById("bodygrid");
  const btn = document.getElementById("chattoggle");
  rail.hidden = !open;
  grid.classList.toggle("chat-open", open);
  btn.classList.toggle("accent", open);
  hideHover();
  if (open) renderChatRail();
}
function renderChatRail() {
  const rail = document.getElementById("chatrail");
  rail.replaceChildren();
  rail.append(el("div", {
    class: "rail-head"
  }, el("span", {
    class: "subhead",
    style: "margin:0"
  }, "chat"), el("span", {
    class: "sql-caption"
  }, state.cfg?.lm ? `${state.db} \xB7 ${state.cfg.lm.model}` : "no model configured")));
  if (!state.cfg?.lm) {
    rail.append(el("div", {
      class: "rail-transcript"
    }, el("div", {
      class: "empty"
    }, "\u2014 talk to the data by proxy needs a model: restart the console with --lm-endpoint http://host:port/v1 --lm-model <name> (any openai-compatible server: vllm, llama.cpp, litellm; bearer token via LM_API_KEY)")));
    return;
  }
  const db = state.db;
  if (!chatLog.has(db)) chatLog.set(db, []);
  const log = chatLog.get(db);
  const transcript = el("div", {
    class: "rail-transcript"
  });
  const input = el("input", {
    class: "input",
    placeholder: `ask ${db} anything\u2026`,
    onkeydown: (e) => {
      if (e.key === "Enter") send();
    }
  });
  const sendBtn = el("button", {
    class: "btn accent",
    onclick: () => send()
  }, "send");
  rail.append(transcript, el("div", {
    class: "rail-inputrow"
  }, input, sendBtn));
  redraw();
  function redraw() {
    transcript.replaceChildren();
    if (!log.length) {
      transcript.append(el("div", {
        class: "empty"
      }, "\u2014 every mention the model uses is pinned through resolve first; tool calls appear here, trajectories open in the main view"));
    }
    for (const m of log) {
      if (m.role === "user") {
        transcript.append(el("div", {
          class: "chat-msg user"
        }, el("div", {
          class: "who"
        }, "you"), el("div", {
          class: "md"
        }, m.content)));
      } else {
        for (const t of m.trail ?? []) transcript.append(renderTrailItem(t));
        transcript.append(el("div", {
          class: "chat-msg"
        }, el("div", {
          class: "who"
        }, "stemma"), el("div", {
          class: "md"
        }, m.content)));
      }
    }
    transcript.scrollTop = transcript.scrollHeight;
  }
  function renderTrailItem(t) {
    if (t.tool === "resolve" && t.trace) {
      const trace = t.trace;
      const d2 = el("details", {
        class: "chat-tool",
        open: ""
      });
      d2.append(el("summary", null, el("span", {
        class: "chip"
      }, "resolve"), `\u201C${trace.query}\u201D \xB7 ${trace.mentions.length} mention${trace.mentions.length === 1 ? "" : "s"}`));
      d2.append(renderMiniTrace(trace));
      d2.append(el("div", {
        style: "margin:3px 0 2px"
      }, el("button", {
        class: "rail-showtraj",
        onclick: () => showTraceInMain(trace)
      }, "full trajectory \u2192")));
      return d2;
    }
    const d = el("details", {
      class: "chat-tool"
    });
    const label = t.tool === "sql" ? `sql \xB7 ${(t.args.query ?? "").slice(0, 60)}` : t.tool;
    d.append(el("summary", null, el("span", {
      class: "chip"
    }, t.tool), label));
    d.append(el("div", {
      class: "tool-body"
    }, JSON.stringify(t.result, null, 2)));
    return d;
  }
  async function send() {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    log.push({
      role: "user",
      content: text
    });
    redraw();
    const wait = el("div", {
      class: "chat-wait"
    }, el("i"), el("i"), el("i"));
    transcript.append(wait);
    sendBtn.setAttribute("disabled", "");
    try {
      const r = await fetch(`/api/db/${db}/chat`, {
        method: "POST",
        headers: {
          "content-type": "application/json"
        },
        body: JSON.stringify({
          messages: log.map((m) => ({
            role: m.role,
            content: m.content
          }))
        })
      });
      const d = await r.json();
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      log.push({
        role: "assistant",
        content: d.message,
        trail: d.trail
      });
      const lastResolve = [
        ...d.trail
      ].reverse().find((t) => t.tool === "resolve" && t.trace);
      if (lastResolve?.trace) showTraceInMain(lastResolve.trace);
    } catch (e) {
      log.push({
        role: "assistant",
        content: "\u2014 " + e.message,
        trail: []
      });
    } finally {
      wait.remove();
      sendBtn.removeAttribute("disabled");
      redraw();
    }
  }
}
async function viewData(host, params, table) {
  const tables = state.schema?.tables ?? [];
  const name = table ?? tables[0]?.name;
  setCrumbs("data", name);
  if (!name) {
    host.append(el("div", {
      class: "empty"
    }, "\u2014 no tables in this database"));
    return;
  }
  const limit = 50;
  const meta = tables.find((t) => t.name === name);
  const cursors = [
    null
  ];
  let lastRowid = null;
  let hasMore = false;
  let filter = params.get("q") ?? "";
  const filterInput = el("input", {
    class: "input",
    value: filter,
    placeholder: "filter \u2014 substring across text columns (trigram-served)\u2026",
    onkeydown: (e) => {
      if (e.key === "Enter") {
        filter = filterInput.value.trim();
        cursors.length = 1;
        load(null);
      }
    }
  });
  const body = el("div", null);
  host.append(el("h1", {
    class: "h1"
  }, name), el("p", {
    class: "sql-caption"
  }, meta ? `~${meta.row_count.toLocaleString()} rows \xB7 ` + meta.columns.map((c) => `${c.name} ${c.type.toLowerCase()}${c.pk ? " \xB7pk" : ""}`).join(" \xB7 ") : ""), el("div", {
    class: "data-tools"
  }, filterInput, el("button", {
    class: "btn",
    onclick: () => {
      filter = filterInput.value.trim();
      cursors.length = 1;
      load(null);
    }
  }, "filter")), body);
  await load(null);
  async function load(after) {
    body.replaceChildren(el("div", {
      class: "empty"
    }, "loading\u2026"));
    const qs = new URLSearchParams({
      limit: String(limit)
    });
    if (after !== null) qs.set("after", String(after));
    if (filter) qs.set("q", filter);
    const d = await getJSON(`/api/db/${state.db}/rows/${encodeURIComponent(name)}?${qs}`);
    hasMore = d.has_more;
    const ridIdx = d.columns.indexOf("_rowid");
    lastRowid = d.rows.length ? Number(d.rows[d.rows.length - 1][ridIdx]) : null;
    const tbl = el("table", {
      class: "grid"
    }, el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))), el("tbody", null, d.rows.map((r) => el("tr", null, r.map((v) => el("td", {
      class: typeof v === "number" ? "num" : null
    }, v === null ? "\u2205" : v))))));
    const pager = el("div", {
      class: "pager"
    }, el("button", {
      class: "btn",
      disabled: cursors.length <= 1 ? "" : null,
      onclick: () => {
        cursors.pop();
        load(cursors[cursors.length - 1]);
      }
    }, "\u2039 prev"), el("button", {
      class: "btn",
      disabled: hasMore ? null : "",
      onclick: () => {
        cursors.push(lastRowid);
        load(lastRowid);
      }
    }, "next \u203A"), el("span", {
      class: "where"
    }, d.rows.length ? `page ${cursors.length} \xB7 ${d.rows.length} rows${filter ? ` \xB7 filtered \u201C${filter}\u201D` : ""}` : "\u2014 nothing matches"));
    body.replaceChildren(el("div", {
      class: "table-scroll"
    }, tbl), pager);
  }
}
var KIND_TOGGLES = [
  "column",
  "value",
  "term"
];
async function viewGraph(host) {
  setCrumbs("graph");
  const g = await getJSON(`/api/db/${state.db}/graph`);
  host.append(el("h1", {
    class: "h1"
  }, "knowledge graph"), el("p", {
    class: "lede"
  }, g.layer === "compiled" ? "compiled from the data: schema (tables, columns, declared keys), discovered relations (dashed \u2014 inclusion-mined joins with confidence), and the profile layer (frequent values, characteristic terms, term co-occurrence). instance-layer entities arrive with collective disambiguation." : "schema layer only \u2014 run stemma-server against this database once to compile the full graph."));
  const detail = el("div", {
    class: "graph-detail",
    hidden: ""
  });
  let selectedKey = null;
  const shown = /* @__PURE__ */ new Set([
    "table",
    ...KIND_TOGGLES
  ]);
  if (g.nodes.length > 160) shown.delete("column");
  const legend = el("div", {
    class: "graph-legend"
  });
  const panel = el("div", {
    class: "panel"
  });
  for (const k of KIND_TOGGLES) {
    const n = g.nodes.filter((x) => x.kind === k).length;
    if (!n) continue;
    const chip = el("button", {
      class: "chip" + (shown.has(k) ? "" : " off"),
      onclick: () => {
        if (shown.has(k)) shown.delete(k);
        else shown.add(k);
        chip.classList.toggle("off");
        draw();
      }
    }, `${k}s \xB7 ${n}`);
    legend.append(chip);
  }
  legend.append(el("span", {
    class: "sql-caption"
  }, "solid = declared \xB7 dashed amber = inferred \xB7 click a node to inspect it"));
  host.append(legend, detail, panel);
  draw();
  function draw() {
    hideHover();
    panel.replaceChildren();
    const nodes = g.nodes.filter((n) => shown.has(n.kind));
    const keys = new Set(nodes.map((n) => n.key));
    const edges = g.edges.filter((e) => keys.has(e.source) && keys.has(e.target));
    if (!nodes.length) {
      panel.append(el("div", {
        class: "empty"
      }, "\u2014 nothing to show"));
      return;
    }
    const tables = nodes.filter((n) => n.kind === "table");
    const W = tables.length > 1 ? 1560 : 1100;
    const H = tables.length > 1 ? 1150 : 900;
    const cx = W / 2, cy = H / 2;
    const pos = /* @__PURE__ */ new Map();
    const R1 = tables.length > 1 ? 300 : 0;
    tables.forEach((n, i) => {
      const a = 2 * Math.PI * i / tables.length - Math.PI / 2;
      pos.set(n.key, {
        x: cx + R1 * Math.cos(a),
        y: cy + R1 * Math.sin(a)
      });
    });
    const childrenOf = (parentKey, kinds) => edges.filter((e) => e.source === parentKey && kinds.includes(nodes.find((n) => n.key === e.target)?.kind ?? "")).map((e) => e.target);
    for (const t of tables) {
      const p = pos.get(t.key);
      const away = Math.atan2(p.y - cy, p.x - cx);
      const base = tables.length > 1 ? away : -Math.PI / 2;
      const kids = [
        ...childrenOf(t.key, [
          "column"
        ]),
        ...childrenOf(t.key, [
          "term"
        ])
      ];
      const spread = tables.length === 1 ? 2 * Math.PI * (1 - 1 / Math.max(2, kids.length)) : Math.min(3, 0.5 * kids.length);
      let placed = 0, ring = 0;
      while (placed < kids.length) {
        const r = (tables.length > 1 ? 175 : 215) + ring * 62;
        const cap = Math.max(6, Math.floor(spread * r / 92));
        const batch = kids.slice(placed, placed + cap);
        batch.forEach((k, i) => {
          const a = base + (batch.length === 1 ? 0 : (i / (batch.length - 1) - 0.5) * spread);
          pos.set(k, {
            x: p.x + r * Math.cos(a),
            y: p.y + r * Math.sin(a)
          });
          for (const [j, v] of childrenOf(k, [
            "value"
          ]).entries()) {
            pos.set(v, {
              x: p.x + (r + 105) * Math.cos(a + (j - 0.5) * 0.16),
              y: p.y + (r + 105) * Math.sin(a + (j - 0.5) * 0.16)
            });
          }
        });
        placed += batch.length;
        ring += 1;
      }
    }
    const svg = svgEl("svg", {
      class: "graph-svg",
      viewBox: `0 0 ${W} ${H}`,
      role: "img",
      "aria-label": "knowledge graph"
    });
    svg.append(svgEl("defs", null, svgEl("marker", {
      id: "arrow",
      viewBox: "0 0 8 8",
      refX: 7,
      refY: 4,
      markerWidth: 6,
      markerHeight: 6,
      orient: "auto"
    }, svgEl("path", {
      d: "M 0 0 L 8 4 L 0 8 z",
      fill: "var(--flat)"
    }))));
    const edgeEls = [];
    for (const e of edges) {
      const a = pos.get(e.source), b = pos.get(e.target);
      if (!a || !b) continue;
      const bend = e.kind === "fk" || e.kind === "inferred_fk" ? 0.12 : 0.02;
      const mx = (a.x + b.x) / 2 + (a.y - b.y) * bend;
      const my = (a.y + b.y) / 2 + (b.x - a.x) * bend;
      const path = svgEl("path", {
        class: `gedge kind-${e.kind}`,
        d: `M ${a.x} ${a.y} Q ${mx} ${my} ${b.x} ${b.y}`,
        ...e.kind === "fk" || e.kind === "inferred_fk" ? {
          "marker-end": "url(#arrow)"
        } : {}
      });
      if (e.label || e.kind === "inferred_fk") {
        const conf = e.props.confidence;
        hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}` + (conf !== void 0 ? ` \xB7 confidence ${conf}` : ""));
      }
      edgeEls.push({
        el: path,
        e
      });
      svg.append(path);
      if (e.kind === "fk" || e.kind === "inferred_fk") {
        svg.append(svgEl("text", {
          class: "gedge-label",
          x: mx,
          y: my,
          "text-anchor": "middle"
        }, e.label));
      }
    }
    const nodeEls = /* @__PURE__ */ new Map();
    for (const n of nodes) {
      const p = pos.get(n.key);
      if (!p) continue;
      const boxed = n.kind === "table" || n.kind === "column";
      let grp;
      if (boxed) {
        const w = Math.max(90, n.label.length * 8 + 22);
        const h = n.kind === "column" ? 26 : 40;
        grp = svgEl("g", {
          class: `gnode kind-${n.kind}`,
          transform: `translate(${p.x - w / 2}, ${p.y - h / 2})`,
          cursor: "pointer"
        }, svgEl("rect", {
          width: w,
          height: h,
          rx: 4
        }));
        if (n.kind === "table") {
          grp.append(svgEl("text", {
            x: w / 2,
            y: 17,
            "text-anchor": "middle"
          }, n.label), svgEl("text", {
            class: "grows",
            x: w / 2,
            y: 31,
            "text-anchor": "middle"
          }, `~${Number(n.props.rows ?? 0).toLocaleString()} rows`));
        } else {
          grp.append(svgEl("text", {
            x: w / 2,
            y: h / 2 + 3.5,
            "text-anchor": "middle"
          }, n.label));
        }
      } else {
        const c = Number(n.props.centrality ?? 0);
        const r = Math.min(6, 2.2 + Math.sqrt(c) * 14);
        grp = svgEl("g", {
          class: `gnode kind-${n.kind}`,
          transform: `translate(${p.x}, ${p.y})`,
          cursor: "pointer"
        }, svgEl("circle", {
          r,
          cx: 0,
          cy: 0
        }), svgEl("text", {
          x: 0,
          y: 15 + r,
          "text-anchor": "middle"
        }, n.label));
      }
      grp.addEventListener("click", (ev) => {
        ev.stopPropagation();
        select(n, grp);
      });
      hov(grp, `<b>${esc(n.label)}</b> \xB7 ${esc(n.kind)}`);
      nodeEls.set(n.key, grp);
      svg.append(grp);
    }
    svg.addEventListener("click", () => select(null, null));
    panel.append(svg);
    if (selectedKey) {
      const n = nodes.find((x) => x.key === selectedKey);
      const gel = n && nodeEls.get(selectedKey);
      if (n && gel) select(n, gel);
      else select(null, null);
    }
    function select(n, gel) {
      hideHover();
      nodeEls.forEach((x) => x.classList.remove("sel"));
      edgeEls.forEach(({ el: x }) => x.classList.remove("hot"));
      if (!n || !gel) {
        selectedKey = null;
        detail.hidden = true;
        return;
      }
      selectedKey = n.key;
      gel.classList.add("sel");
      const touching = edgeEls.filter(({ e }) => e.source === n.key || e.target === n.key);
      touching.forEach(({ el: x }) => x.classList.add("hot"));
      detail.hidden = false;
      detail.replaceChildren(el("span", {
        class: "kindtag"
      }, n.kind), el("span", {
        class: "name"
      }, n.label), el("span", {
        class: "props"
      }, Object.entries(n.props).map(([k, v]) => `${k} ${v}`).join(" \xB7 ") || "\u2014"), el("span", {
        class: "props"
      }, `${touching.length} edge${touching.length === 1 ? "" : "s"}`));
      if (n.kind === "table") {
        detail.append(el("button", {
          class: "btn accent",
          onclick: () => {
            location.hash = "#/data/" + encodeURIComponent(n.label);
          }
        }, "browse data \u2192"));
      } else if (n.kind === "term" || n.kind === "value") {
        const neighbors = touching.filter(({ e }) => e.kind === "cooccurs").map(({ e }) => {
          const other = e.source === n.key ? e.target : e.source;
          return g.nodes.find((x) => x.key === other)?.label ?? "";
        }).filter(Boolean);
        if (neighbors.length) {
          detail.append(el("span", {
            class: "props"
          }, `co-occurs: ${neighbors.join(" \xB7 ")}`));
        }
        detail.append(el("button", {
          class: "btn accent",
          onclick: () => {
            location.hash = "#/query?d=nl&q=" + encodeURIComponent(n.label);
          }
        }, `resolve \u201C${n.label}\u201D \u2192`));
      }
    }
  }
}
async function route() {
  const hash = location.hash || "#/query";
  const [path, qs] = hash.slice(2).split("?");
  const [view, arg] = path.split("/");
  const params = new URLSearchParams(qs ?? "");
  hideHover();
  closeAllMenus();
  if (view === "chat") {
    setChatRail(true);
    location.hash = "#/query";
    return;
  }
  const mapped = view === "resolve" ? "query" : view === "sql" ? "query" : view || "query";
  if (view === "sql") params.set("d", "sql");
  state.view = mapped;
  document.querySelectorAll("#nav a").forEach((a) => a.classList.toggle("on", a.dataset.view === state.view));
  if (!state.schema && state.db) {
    try {
      state.schema = await getJSON(`/api/db/${state.db}/schema`);
    } catch (_e) {
      state.schema = {
        tables: []
      };
    }
  }
  renderSidebar();
  const host = document.getElementById("view");
  host.replaceChildren();
  try {
    if (state.view === "data") await viewData(host, params, arg ? decodeURIComponent(arg) : void 0);
    else if (state.view === "graph") await viewGraph(host);
    else viewQuery(host, params);
  } catch (e) {
    host.append(el("div", {
      class: "sql-error"
    }, e.message));
  }
}
(async function boot() {
  initPickers();
  const cfg = await getJSON("/api/config");
  state.cfg = cfg;
  state.dbs = cfg.databases;
  state.db = cfg.databases[0] ?? null;
  document.getElementById("topsearch").addEventListener("keydown", (ev) => {
    const e = ev;
    const target = ev.target;
    if (e.key === "Enter" && target.value.trim()) {
      location.hash = "#/query?d=nl&q=" + encodeURIComponent(target.value);
    }
  });
  document.getElementById("chattoggle").addEventListener("click", () => setChatRail(Boolean(document.getElementById("chatrail").hidden)));
  if (chatRailOpen()) setChatRail(true);
  globalThis.addEventListener("hashchange", route);
  pollHealth();
  route();
})();
