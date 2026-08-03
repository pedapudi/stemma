# Bibliography

Shared reference list for the [technical design](README.md) documents.

Entries in sections A–G were verified against primary sources — arXiv abstract
pages, the ACL Anthology, DBLP, AAAI/NeurIPS/ICLR/PMLR proceedings, PVLDB
PDFs, Crossref, JMLR — rather than cited from memory. Titles, author lists,
venues of record and identifiers are as published; where a work appeared as a
preprint and later at a venue, the venue of record is given and the preprint
identifier retained.

Section H is different, and is labelled as such: those entries were checked
against ambit's own reference list rather than independently.

**Citation keys are disambiguated by first-author given name where surnames
collide.** There are two Gaos, three Lis, two Wangs, two Lees and two
Papenbrocks below. `[J. Gao 2019]` and `[T. Gao 2021]` are different people,
as are `[B.Z. Li 2020]`, `[Bohan Li 2020]`, `[H. Li 2023]`, `[J. Li 2023]`
and `[C. Li 2025]`.

---

## A. Entity linking

- **[Wu 2020]** Ledell Wu, Fabio Petroni, Martin Josifoski, Sebastian Riedel,
  Luke Zettlemoyer. "Scalable Zero-shot Entity Linking with Dense Entity
  Retrieval." EMNLP 2020, 6397–6407. arXiv:1911.03814.
  DOI 10.18653/v1/2020.emnlp-main.519. *(BLINK: bi-encoder retrieval followed
  by cross-encoder ranking — the canonical retrieve-then-rerank entity linker.
  "Scalable" was added between the 2019 preprint and the camera-ready.)*
- **[B.Z. Li 2020]** Belinda Z. Li, Sewon Min, Srinivasan Iyer, Yashar Mehdad,
  Wen-tau Yih. "Efficient One-Pass End-to-End Entity Linking for Questions."
  EMNLP 2020, 6433–6441. arXiv:2010.02413.
  DOI 10.18653/v1/2020.emnlp-main.522. *(ELQ: joint mention detection and
  linking in one bi-encoder pass, aimed at questions rather than documents.)*
- **[De Cao 2021]** Nicola De Cao, Gautier Izacard, Sebastian Riedel, Fabio
  Petroni. "Autoregressive Entity Retrieval." ICLR 2021 (spotlight).
  arXiv:2010.00904. *(GENRE: generates entity names under a prefix-trie
  constraint — the generative alternative stemma argues against for changing
  catalogs.)*
- **[Ayoola 2022]** Tom Ayoola, Shubhi Tyagi, Joseph Fisher, Christos
  Christodoulopoulos, Andrea Pierleoni. "ReFinED: An Efficient
  Zero-shot-capable Approach to End-to-End Entity Linking." NAACL-HLT 2022
  Industry Track, 209–220. arXiv:2207.04108.
  DOI 10.18653/v1/2022.naacl-industry.24.
- **[Orlando 2024]** Riccardo Orlando, Pere-Lluís Huguet Cabot, Edoardo Barba,
  Roberto Navigli. "ReLiK: Retrieve and LinK, Fast and Accurate Entity Linking
  and Relation Extraction on an Academic Budget." Findings of ACL 2024,
  14114–14132. arXiv:2408.00103. DOI 10.18653/v1/2024.findings-acl.839.
  *(Findings, not the main track. The repository's "Retrieve, Read and Link"
  phrasing is not the published title.)*
- **[Xin 2025]** Amy Xin, Yunjia Qi, Zijun Yao, Fangwei Zhu, Kaisheng Zeng,
  Bin Xu, Lei Hou, Juanzi Li. "LLMAEL: Large Language Models are Good Context
  Augmenters for Entity Linking." CIKM 2025, 3550–3559. arXiv:2407.04020.
  DOI 10.1145/3746252.3761156. *(The mention-expansion result. The headline
  +8.9% is an absolute entity-linking accuracy gain **over prior tuning-free
  LLM-integration methods**, aggregated across six EL benchmarks — not over
  specialized entity linkers in general.)*
- **[Hoffart 2011]** Johannes Hoffart, Mohamed Amir Yosef, Ilaria Bordino,
  Hagen Fürstenau, Manfred Pinkal, Marc Spaniol, Bilyana Taneva, Stefan
  Thater, Gerhard Weikum. "Robust Disambiguation of Named Entities in Text."
  EMNLP 2011, 782–792. ACL Anthology D11-1072. *(AIDA: collective
  disambiguation as dense-subgraph selection over a mention–entity graph.)*
- **[Phan 2019]** Minh C. Phan, Aixin Sun, Yi Tay, Jialong Han, Chenliang Li.
  "Pair-Linking for Collective Entity Disambiguation: Two Could Be Better Than
  All." IEEE Transactions on Knowledge and Data Engineering 31(7):1383–1396,
  2019. arXiv:1802.01074. DOI 10.1109/TKDE.2018.2857493. *(A journal paper,
  and "Disambiguation" not "Linking". Argues entity–entity coherence is
  sparser than joint models assume, and that pairwise linking with a
  minimum-spanning-tree objective beats all-mention joint optimization.)*

## B. The case against constrained generative retrieval

- **[S. Wu 2025]** Shiguang Wu, Zhaochun Ren, Xin Xin, Jiyuan Yang, Mengqi
  Zhang, Zhumin Chen, Maarten de Rijke, Pengjie Ren. "Constrained
  Auto-Regressive Decoding Constrains Generative Retrieval." SIGIR 2025,
  2429–2440. arXiv:2504.09935. DOI 10.1145/3726302.3729934.
  *(A **theoretical** paper, not an empirical ablation. It derives a lower
  bound on the KL divergence between true and predicted step-wise marginals,
  because the model is unaware of future constraints while generating; and it
  shows that beam search over those marginals optimizes the wrong objective,
  so on sparse relevance distributions a model can reach perfect top-1
  precision while suffering poor top-k recall. Validated on MS MARCO V1
  passages and evaluated on MS MARCO-dev and TREC DL 2019/2020, where the
  precision/recall gap opens at the very first decoding step: R@50/P@1 of
  53.7/69.8 on DL19, 63.3/75.9 on DL20, 67.5/90.5 on MS MARCO-dev.)*

## C. Text-to-SQL: schema and value linking

- **[B. Wang 2020]** Bailin Wang, Richard Shin, Xiaodong Liu, Oleksandr
  Polozov, Matthew Richardson. "RAT-SQL: Relation-Aware Schema Encoding and
  Linking for Text-to-SQL Parsers." ACL 2020, 7567–7578. arXiv:1911.04942.
  DOI 10.18653/v1/2020.acl-main.677.
- **[Lin 2020]** Xi Victoria Lin, Richard Socher, Caiming Xiong. "Bridging
  Textual and Tabular Data for Cross-Domain Text-to-SQL Semantic Parsing."
  Findings of EMNLP 2020, 4870–4888. arXiv:2012.12627.
  DOI 10.18653/v1/2020.findings-emnlp.438. *(BRIDGE: schema serialized as a
  tagged sequence concatenated to the question, with anchor text from matched
  cell values — the value-linking-as-input pattern.)*
- **[H. Li 2023]** Haoyang Li, Jing Zhang, Cuiping Li, Hong Chen. "RESDSQL:
  Decoupling Schema Linking and Skeleton Parsing for Text-to-SQL." AAAI 2023,
  37(11):13067–13075. arXiv:2302.05965. DOI 10.1609/aaai.v37i11.26535.
- **[Talaei 2024]** Shayan Talaei, Mohammadreza Pourreza, Yu-Chen Chang,
  Azalia Mirhoseini, Amin Saberi. "CHESS: Contextual Harnessing for Efficient
  SQL Synthesis." arXiv:2405.16755, 2024. *(Preprint only — an ICML 2025
  workshop poster is its sole non-preprint appearance; it is **not** a VLDB
  paper. Hierarchical retrieval over data catalogs and database values plus
  adaptive schema pruning — resolve-then-generate inside a text-to-SQL
  pipeline.)*
- **[Nan 2026]** Yafeng Nan, Haifeng Sun, Zirui Zhuang, Qi Qi, Guojun Chu,
  Jianxin Liao, Dan Pei, Jingyu Wang. "DIVER: A Robust Text-to-SQL System with
  Dynamic Interactive Value Linking and Evidence Reasoning." SIGMOD 2026.
  arXiv:2602.12064. *(**The strongest published evidence for stemma's
  premise.** Measures a collapse of over 10 points of execution accuracy when
  expert evidence is unavailable: CodeS-1B 50.46→38.46, CodeS-3B 55.02→43.42,
  CodeS-7B 57.17→45.24. Also notes that only 5 of 52 BIRD leaderboard methods
  report no-evidence numbers at all.)*
- **[Yun 2025]** Janghyeon Yun, Sang-goo Lee. "SEED: Enhancing Text-to-SQL
  Performance and Practical Usability Through Automatic Evidence Generation."
  IEEE ICDE Workshops (ICDEW) 2025. arXiv:2506.07423.
  DOI 10.1109/ICDEW67478.2025.00005. *(The same ablation from the other side:
  removing human evidence costs 8.35–20.86 EX points across systems —
  RSL-SQL/GPT-4o 65.78→54.50, DAIL-SQL/GPT-4 56.32→35.46 — and automatically
  generated evidence recovers much of it. Also documents defects in BIRD's
  shipped human evidence, which is why it is a reference and not an oracle.)*
- **[Maamari 2024]** Karime Maamari, Fadhil Abubaker, Daniel Jaroslawicz,
  Amine Mhedhbi. "The Death of Schema Linking? Text-to-SQL in the Age of
  Well-Reasoned Language Models." arXiv:2408.07702, 2024. *(Preprint only, and
  not a BIRD-community publication — three of four authors are at Distyl AI.
  Its claim is that modern reasoning models tolerate irrelevant schema in
  context, so aggressive schema-linking **filters** mostly hurt by dropping
  required columns; their pipeline forgoes schema linking entirely when the
  schema fits the context window, reaching 71.83% on BIRD.)*
- **[J. Li 2023]** Jinyang Li, Binyuan Hui, Ge Qu, Jiaxi Yang, Binhua Li,
  Bowen Li, Bailin Wang, Bowen Qin, Rongyu Cao, Ruiying Geng, Nan Huo, Xuanhe
  Zhou, Chenhao Ma, Guoliang Li, Kevin C.C. Chang, Fei Huang, Reynold Cheng,
  Yongbin Li. "Can LLM Already Serve as A Database Interface? A BIg Bench for
  Large-Scale Database Grounded Text-to-SQLs." NeurIPS 2023 Datasets and
  Benchmarks Track. arXiv:2305.03111. *(BIRD, and the source of the human
  `evidence` strings whose removal defines stemma's evaluation setting.)*
- **[Lei 2025]** Fangyu Lei, Jixuan Chen, Yuxiao Ye, Ruisheng Cao, Dongchan
  Shin, Hongjin Su, Zhaoqing Suo, Hongcheng Gao, Wenjing Hu, Pengcheng Yin,
  Victor Zhong, Caiming Xiong, Ruoxi Sun, Qian Liu, Sida Wang, Tao Yu.
  "Spider 2.0: Evaluating Language Models on Real-World Enterprise Text-to-SQL
  Workflows." ICLR 2025 (Oral). arXiv:2411.07763.
- **[C. Li 2025]** Chaofan Li, Yingxia Shao, Yawen Li, Zheng Liu. "SEA-SQL:
  Semantic-Enhanced Text-to-SQL with Adaptive Refinement." *Frontiers of
  Computer Science*, 2025. arXiv:2408.04919. DOI 10.1007/s11704-025-41136-3.
  *(The only published source cleanly supporting a figure in the 30–40% band:
  "schema linking errors are the most common, accounting for 37% of errors in
  the BIRD dataset", with the category explicitly encompassing incorrect
  tables, columns **or values**. The denominator is SEA-SQL's own errors on
  BIRD dev, not BIRD failures in general.)*
- **[D. Lee 2025]** Dongjun Lee, Choongwon Park, Jaehyuk Kim, Heesoo Park.
  "MCS-SQL: Leveraging Multiple Prompts and Multiple-Choice Selection For
  Text-to-SQL Generation." COLING 2025, 337–353. arXiv:2405.07467.
  *(The contrasting datapoint: schema linking is 20% of 100 sampled BIRD-dev
  failures — but 62% of those "failures" turned out to be bad gold or
  semantically correct SQL, which puts schema linking at roughly 53% of
  genuine model errors. The spread between this and SEA-SQL is a taxonomy and
  denominator disagreement, not a measurement disagreement.)*
- **[Qu 2024]** Ge Qu, Jinyang Li, Bowen Li, Bowen Qin, Nan Huo, Chenhao Ma,
  Reynold Cheng. "Before Generation, Align it! A Novel and Effective Strategy
  for Mitigating Hallucinations in Text-to-SQL Generation." Findings of ACL
  2024. arXiv:2405.15307. *(The closest published proxy for a **value**-linking
  error rate specifically: "Value Misrepresentation" at 24%, alongside
  Attribute Overanalysis 49% and Schema Contradiction 30%. The percentages sum
  past 100% and the denominator is unstated, so treat it as soft evidence.)*
- **[C.-H. Lee 2021]** Chia-Hsuan Lee, Oleksandr Polozov, Matthew Richardson.
  "KaggleDBQA: Realistic Evaluation of Text-to-SQL Parsers." ACL-IJCNLP 2021
  (Long Papers), 2261–2273. arXiv:2106.11455.
  DOI 10.18653/v1/2021.acl-long.176.

## D. Keyword extraction and graph ranking

- **[Mihalcea 2004]** Rada Mihalcea, Paul Tarau. "TextRank: Bringing Order
  into Text." EMNLP 2004, 404–411. ACL Anthology W04-3252. *(Singular "Text".
  PageRank over a word co-occurrence graph for unsupervised keyword and
  sentence extraction.)*
- **[Page 1999]** Lawrence Page, Sergey Brin, Rajeev Motwani, Terry Winograd.
  "The PageRank Citation Ranking: Bringing Order to the Web." Stanford InfoLab
  Technical Report 1999-66, 1999. *(The Stanford record dates it 11 Nov 1999;
  the manuscript's title page is dated 29 Jan 1998 under an earlier number,
  which is why both years circulate. 1999 matches the record.)*
- **[Rose 2010]** Stuart Rose, Dave Engel, Nick Cramer, Wendy Cowley.
  "Automatic Keyword Extraction from Individual Documents." Ch. 1 in
  M. W. Berry & J. Kogan (eds.), *Text Mining: Applications and Theory*,
  Wiley, 2010, 1–20. DOI 10.1002/9780470689646.ch1. *(RAKE. Four authors —
  Cramer is frequently dropped in citations.)*
- **[Campos 2020]** Ricardo Campos, Vítor Mangaravite, Arian Pasquali, Alípio
  Jorge, Célia Nunes, Adam Jatowt. "YAKE! Keyword extraction from single
  documents using multiple local features." *Information Sciences*
  509:257–289, 2020. DOI 10.1016/j.ins.2019.09.013.
- **[Campos 2018]** Ricardo Campos, Vítor Mangaravite, Arian Pasquali, Alípio
  Mário Jorge, Célia Nunes, Adam Jatowt. "A Text Feature Based Automatic
  Keyword Extraction Method for Single Documents." ECIR 2018, LNCS 10772,
  684–691. DOI 10.1007/978-3-319-76941-7_63. *(The earlier YAKE short paper;
  its title does not contain "YAKE".)*
- **[Wan 2008]** Xiaojun Wan, Jianguo Xiao. "Single Document Keyphrase
  Extraction Using Neighborhood Knowledge." AAAI 2008, 855–860. *(Introduces
  SingleRank as the single-document baseline to ExpandRank.)*
- **[Florescu 2017]** Corina Florescu, Cornelia Caragea. "PositionRank: An
  Unsupervised Approach to Keyphrase Extraction from Scholarly Documents."
  ACL 2017 (Long Papers), 1105–1115. DOI 10.18653/v1/P17-1102.

## E. Inclusion dependencies and foreign-key discovery

- **[Bauckmann 2007]** Jana Bauckmann, Ulf Leser, Felix Naumann, Véronique
  Tietz. "Efficiently Detecting Inclusion Dependencies." ICDE 2007, 1448–1450.
  DOI 10.1109/ICDE.2007.369032. *(The **SPIDER** algorithm — Single Pass
  Inclusion DEpendency Recognition. A three-page short paper. **Unrelated to
  the Spider text-to-SQL benchmark**, with which it shares only a name.)*
- **[Papenbrock 2015a]** Thorsten Papenbrock, Tanja Bergmann, Moritz Finke,
  Jakob Zwiener, Felix Naumann. "Data Profiling with Metanome."
  PVLDB 8(12):1860–1863, 2015. DOI 10.14778/2824032.2824086.
- **[Papenbrock 2015b]** Thorsten Papenbrock, Sebastian Kruse, Jorge-Arnulfo
  Quiané-Ruiz, Felix Naumann. "Divide & Conquer-based Inclusion Dependency
  Discovery." PVLDB 8(7):774–785, 2015. DOI 10.14778/2752939.2752946.
  *(BINDER — the scalable IND discovery algorithm, and the pruning strategy
  stemma's naive `EXCEPT`-based mining does not implement.)*
- **[Rostin 2009]** Alexandra Rostin, Oliver Albrecht, Jana Bauckmann, Felix
  Naumann, Ulf Leser. "A Machine Learning Approach to Foreign Key Discovery."
  WebDB 2009 (co-located with SIGMOD). *(Establishes that the set of valid
  inclusion dependencies contains many spurious set inclusions, and classifies
  true foreign keys from IND features — the reason stemma marks discovered
  joins `inferred` with a confidence rather than treating them as
  constraints.)*
- **[Jiang 2020]** Lan Jiang, Felix Naumann. "Holistic primary key and foreign
  key detection." *Journal of Intelligent Information Systems* 54(3):439–461,
  2020. DOI 10.1007/s10844-019-00562-z. *(HoPF — two authors. Score functions
  that extract true PKs and FKs from the much larger sets of valid unique
  column combinations and inclusion dependencies.)*

## F. Retrieval and fusion

- **[Cormack 2009]** Gordon V. Cormack, Charles L. A. Clarke, Stefan Büttcher.
  "Reciprocal rank fusion outperforms condorcet and individual rank learning
  methods." SIGIR 2009, 758–759. DOI 10.1145/1571941.1572114. *(Two pages, one
  formula: Σ 1/(k + rank). No score normalization, no training data, and it
  beats the runs it fuses. stemma uses k = 4 rather than the paper's 60 — see
  [03-resolution.md](03-resolution.md#the-fused-base).)*
- **[Robertson 2009]** Stephen E. Robertson, Hugo Zaragoza. "The Probabilistic
  Relevance Framework: BM25 and Beyond." *Foundations and Trends in
  Information Retrieval* 3(4):333–389, 2009. DOI 10.1561/1500000019.
- **[Paulsen 2023]** Derek Paulsen, Yash Govind, AnHai Doan. "Sparkly: A
  Simple yet Surprisingly Strong TF/IDF Blocker for Entity Matching."
  PVLDB 16(6):1507–1519, 2023. DOI 10.14778/3583140.3583163. *(The paper calls
  itself a **TF/IDF** blocker; BM25 is the Lucene scoring function it uses. It
  builds a Lucene inverted index over one table and runs distributed top-k
  BM25 lookups for the other across a Spark cluster, beating eight
  state-of-the-art blockers on fifteen datasets — the evidence that lexical
  retrieval remains a strong baseline for entity matching.)*
- **[Edge 2024]** Darren Edge, Ha Trinh, Newman Cheng, Joshua Bradley, Alex
  Chao, Apurva Mody, Steven Truitt, Dasha Metropolitansky, Robert Osazuwa
  Ness, Jonathan Larson. "From Local to Global: A Graph RAG Approach to
  Query-Focused Summarization." arXiv:2404.16130, 2024. *(Preprint. Graph
  structure over a corpus for retrieval, requiring an LLM extraction pass per
  document — which stemma's deterministic term and phrase mining is the cheap
  first-order substitute for.)*
- **[Guo 2025]** Zirui Guo, Lianghao Xia, Yanhua Yu, Tu Ao, Chao Huang.
  "LightRAG: Simple and Fast Retrieval-Augmented Generation." Findings of
  EMNLP 2025, 10746–10761. arXiv:2410.05779.
  DOI 10.18653/v1/2025.findings-emnlp.568. *(Secondary sources dating this to
  EMNLP 2024 are wrong.)*

## G. Embedding geometry: anisotropy, crowding, hubness

- **[Ethayarajh 2019]** Kawin Ethayarajh. "How Contextual are Contextualized
  Word Representations? Comparing the Geometry of BERT, ELMo, and GPT-2
  Embeddings." EMNLP-IJCNLP 2019, 55–65. arXiv:1909.00512.
  DOI 10.18653/v1/D19-1006. *(Contextual representations occupy a narrow cone:
  two random words have far higher expected cosine similarity than chance.)*
- **[J. Gao 2019]** Jun Gao, Di He, Xu Tan, Tao Qin, Liwei Wang, Tie-Yan Liu.
  "Representation Degeneration Problem in Training Natural Language Generation
  Models." ICLR 2019. arXiv:1907.12009. *(The cone is produced by the
  likelihood objective itself, not by the data — which is why it is the
  expected behaviour of a general-purpose encoder rather than a defect.)*
- **[Bohan Li 2020]** Bohan Li, Hao Zhou, Junxian He, Mingxuan Wang, Yiming
  Yang, Lei Li. "On the Sentence Embeddings from Pre-trained Language Models."
  EMNLP 2020, 9119–9130. arXiv:2011.05864.
  DOI 10.18653/v1/2020.emnlp-main.733. *(BERT-flow.)*
- **[Su 2021]** Jianlin Su, Jiarun Cao, Weijie Liu, Yangyiwen Ou. "Whitening
  Sentence Representations for Better Semantics and Faster Retrieval."
  arXiv:2103.15316, 2021. *(Preprint only, no DOI. Most of BERT-flow's gain
  from a linear whitening transform.)*
- **[Huang 2021]** Junjie Huang, Duyu Tang, Wanjun Zhong, Shuai Lu, Linjun
  Shou, Ming Gong, Daxin Jiang, Nan Duan. "WhiteningBERT: An Easy Unsupervised
  Sentence Embedding Approach." Findings of EMNLP 2021, 238–244.
  arXiv:2104.01767. DOI 10.18653/v1/2021.findings-emnlp.23. *(The
  peer-reviewed cousin of [Su 2021]; cite the two together.)*
- **[T. Gao 2021]** Tianyu Gao, Xingcheng Yao, Danqi Chen. "SimCSE: Simple
  Contrastive Learning of Sentence Embeddings." EMNLP 2021, 6894–6910.
  arXiv:2104.08821. DOI 10.18653/v1/2021.emnlp-main.552.
- **[T. Wang 2020]** Tongzhou Wang, Phillip Isola. "Understanding Contrastive
  Representation Learning through Alignment and Uniformity on the Hypersphere."
  ICML 2020, PMLR 119:9929–9939. arXiv:2005.10242. *(The alignment/uniformity
  decomposition. Its uniformity functional at t = 2 is, through a Chernoff
  bound, exactly a collision bound at σ = 0.25 — see
  [05-encoders-decoders.md](05-encoders-decoders.md#what-ambit-measures-and-why-it-is-exact-in-stemmas-regime).)*
- **[Radovanović 2010]** Miloš Radovanović, Alexandros Nanopoulos, Mirjana
  Ivanović. "Hubs in Space: Popular Nearest Neighbors in High-Dimensional
  Data." *Journal of Machine Learning Research* 11:2487–2531, 2010.
  *(Hubness: a few points become the nearest neighbour of disproportionately
  many queries.)*
- **[Kusupati 2022]** Aditya Kusupati, Gantavya Bhatt, Aniket Rege, Matthew
  Wallingford, Aditya Sinha, Vivek Ramanujan, William Howard-Snyder, Kaifeng
  Chen, Sham Kakade, Prateek Jain, Ali Farhadi. "Matryoshka Representation
  Learning." NeurIPS 2022. arXiv:2205.13147. *(The arXiv v1 title was
  "Matryoshka Representations for Adaptive Deployment"; the NeurIPS title is
  the one of record.)*
- **[Zhang 2025]** Yanzhao Zhang, Mingxin Li, Dingkun Long, Xin Zhang, Huan
  Lin, Baosong Yang, Pengjun Xie, An Yang, Dayiheng Liu, Junyang Lin, Fei
  Huang, Jingren Zhou. "Qwen3 Embedding: Advancing Text Embedding and Reranking
  Through Foundation Models." arXiv:2506.05176, 2025. *(Three sizes — 0.6B at
  1024 dimensions, 4B at 2560, 8B at 4096, all with 32K context. The 0.6B
  maximum output dimension is indeed 1024, with Matryoshka truncation to any
  size from 32 to 1024. This is the base encoder for the legal corpus
  vectors.)*

## H. Additional geometry references

Checked for title, venue and year against
[ambit](https://github.com/pedapudi/ambit)'s own reference list, which carries
ACL Anthology and arXiv links for each. **Not independently re-verified**
against primary sources, unlike sections A–G.

- **[Mu 2018]** Jiaqi Mu, Suma Bhat, Pramod Viswanath. "All-but-the-Top:
  Simple and Effective Postprocessing for Word Representations." ICLR 2018.
  arXiv:1702.01417. *(Dominant-direction removal — with mean-centering, the
  cheapest crowding repair, and the first thing ambit's decision tree tries.)*
- **[Timkey 2021]** William Timkey, Marten van Schijndel. "All Bark and No
  Bite: Rogue Dimensions in Transformer Language Models Obscure
  Representational Quality." EMNLP 2021. arXiv:2109.04404.
- **[Cai 2021]** Xingyu Cai, Jiaji Huang, Yuchen Bian, Kenneth Church.
  "Isotropy in the Contextual Embedding Space: Clusters and Manifolds."
  ICLR 2021.
- **[Godey 2024]** Nathan Godey, Éric de la Clergerie, Benoît Sagot.
  "Anisotropy Is Inherent to Self-Attention in Transformers." EACL 2024.
  arXiv:2401.12143.
- **[Jing 2022]** Li Jing, Pascal Vincent, Yann LeCun, Yuandong Tian.
  "Understanding Dimensional Collapse in Contrastive Self-Supervised
  Learning." ICLR 2022. arXiv:2110.09348.
- **[Roy 2007]** Olivier Roy, Martin Vetterli. "The Effective Rank: A Measure
  of Effective Dimensionality." EUSIPCO 2007.
- **[Rudman 2022]** William Rudman, Nate Gillman, Taylor Rayne, Carsten
  Eickhoff. "IsoScore: Measuring the Uniformity of Embedding Space
  Utilization." Findings of ACL 2022. arXiv:2108.07344.
- **[Steck 2024]** Harald Steck, Chaitanya Ekanadham, Nathan Kallus. "Is
  Cosine-Similarity of Embeddings Really About Similarity?" WWW 2024
  Companion. arXiv:2403.05440.
- **[Xiang 2025]** Yilin Xiang et al. "When to use Graphs in RAG: A
  Comprehensive Analysis for Graph Retrieval-Augmented Generation"
  (GraphRAG-Bench). arXiv:2506.05690. *(Tiered tasks by evidence topology;
  pipeline-layer metrics; the finding that graph structure helps multi-hop
  and synthesis tiers while vanilla RAG matches or beats it on simple fact
  retrieval — the discipline behind 07's per-tier matrix and containment
  grading.)*
- **[Myllymäki 2017]** Mari Myllymäki, Tomáš Mrkvička, Pavel Grabarnik, Henri
  Seijo, Ulf Hahn. "Global envelope tests for spatial processes." *Journal of
  the Royal Statistical Society Series B* 79(2):381–404, 2017. *(The
  rank-envelope test ambit uses to gate its crowding-onset annotation.)*

---

## Notes on contested claims

**The BIRD schema-linking error share is not a settled number, and this
document set does not treat it as one.** The frequently quoted "~37% of BIRD
failures are schema/value linking" traces to exactly one published analysis:
[C. Li 2025] (SEA-SQL) reports that "schema linking errors are the most
common, accounting for 37% of errors in the BIRD dataset", with the category
defined to include incorrect tables, columns or values. That denominator is
*SEA-SQL's own errors on BIRD dev*, not a property of BIRD failures in
general. Other published analyses put the figure anywhere from 20% to 57%
depending on taxonomy and denominator: [D. Lee 2025] reports 20% of sampled
failures, which becomes roughly 53% of *genuine* model errors once bad gold
and semantically-correct SQL are excluded, and [Talaei 2024] reports about 26%
for a vanilla GPT-4 baseline. Two other figures circulate that are not this
number at all — "Rethinking Schema Linking" reports 37% for *missed explicit
columns as a share of schema-linking failures only*, and LinkAlign's widely
quoted 68.3% is Spider in a multi-database retrieval setting.

**The load-bearing evidence for stemma's premise is the no-evidence ablation,
not the error taxonomy.** [Nan 2026] and [Yun 2025] both measure what happens
when the human hints are removed — over 10 points of execution accuracy in the
first case, 8.35 to 20.86 points in the second — and those are direct
measurements of the thing stemma is built to supply, on the benchmark's own
metric. This document set leads with them.

**No published work reports "value linking" as an error category with a
percentage.** The nearest equivalents are [Qu 2024]'s "Value
Misrepresentation" at 24% and [C. Li 2025]'s decision to fold values into the
schema-linking category.

**[Maamari 2024] is not a refutation of stemma's premise.** Its claim is that
*pruning* a schema before generation can cost more recall than the saved
context is worth. stemma is not a filter and removes nothing from a
generator's view; it answers which stored record a phrase denotes, which is a
retrieval question that a longer context window does not answer.
