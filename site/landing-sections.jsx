/* Landing page sections */

const cls = (...xs) => xs.filter(Boolean).join(' ');

/* —— Animated terminal —————————————————————————— */
const TerminalDemo = () => {
  const SCRIPT = [
    { kind: 'cmd',  text: 'ctxd write --subject /work/acme/notes/standup \\\n     --type ctx.note --data \'{"content":"Ship auth Friday"}\'' },
    { kind: 'out',  text: '✓ wrote 019756a3-1234-7000-8000-000000000001' },
    { kind: 'out',  text: '  predecessor_hash: a1b2c3d4e5f6…' },
    { kind: 'cmd',  text: 'ctxd write --subject /work/acme/customers/cust-42 \\\n     --type ctx.crm --data \'{"plan":"enterprise"}\'' },
    { kind: 'out',  text: '✓ wrote 019756a3-1234-7000-8000-000000000002' },
    { kind: 'cmd',  text: 'ctxd read --subject /work/acme --recursive' },
    { kind: 'out',  text: '[\n  { subject: "/work/acme/notes/standup",      type: "ctx.note" },\n  { subject: "/work/acme/customers/cust-42",  type: "ctx.crm"  }\n]' },
    { kind: 'cmd',  text: 'ctxd grant --subject "/work/acme/**" --operations "read,subjects"' },
    { kind: 'out',  text: 'EpAB...biscuit-token...A4Yg' },
    { kind: 'cmd',  text: 'ctxd serve' },
    { kind: 'out',  text: '◆ HTTP admin   127.0.0.1:7777\n◆ Wire         127.0.0.1:7778\n◆ MCP stdio    ready  →  claude desktop / cursor / custom' },
  ];

  const [step, setStep] = React.useState(0);
  const [typed, setTyped] = React.useState('');
  const [done, setDone] = React.useState([]);

  React.useEffect(() => {
    if (step >= SCRIPT.length) {
      const t = setTimeout(() => { setStep(0); setTyped(''); setDone([]); }, 4500);
      return () => clearTimeout(t);
    }
    const cur = SCRIPT[step];
    if (cur.kind === 'cmd') {
      let i = 0;
      const id = setInterval(() => {
        i += 2;
        setTyped(cur.text.slice(0, i));
        if (i >= cur.text.length) {
          clearInterval(id);
          setTimeout(() => {
            setDone(d => [...d, cur]);
            setTyped('');
            setStep(s => s + 1);
          }, 350);
        }
      }, 18);
      return () => clearInterval(id);
    } else {
      const t = setTimeout(() => {
        setDone(d => [...d, cur]);
        setStep(s => s + 1);
      }, 420);
      return () => clearTimeout(t);
    }
  }, [step]);

  return (
    <div className="terminal">
      <div className="terminal__bar">
        <span className="terminal__dot"></span>
        <span className="terminal__dot"></span>
        <span className="terminal__dot"></span>
        <span className="terminal__title">~/ctxd  —  zsh  —  120×40</span>
        <span className="terminal__meta">live</span>
      </div>
      <div className="terminal__body">
        {done.map((line, i) => (
          <div key={i} className={cls('tline', `tline--${line.kind}`)}>
            {line.kind === 'cmd' && <span className="tline__prompt">$</span>}
            <span className="tline__text">{line.text}</span>
          </div>
        ))}
        {step < SCRIPT.length && SCRIPT[step].kind === 'cmd' && (
          <div className="tline tline--cmd">
            <span className="tline__prompt">$</span>
            <span className="tline__text">{typed}<span className="caret">▌</span></span>
          </div>
        )}
      </div>
    </div>
  );
};

/* —— Architecture diagram ——————————————————————
   All-stroke schematic. No big filled blocks. Animated packets flow
   along the data path so it reads like a system monitor, not a slide.
*/
const ArchBox = ({ x, y, w, h, label, sub, accent, kbd, glyph }) => (
  <g>
    <rect x={x} y={y} width={w} height={h}
          fill="var(--paper-2)"
          stroke={accent ? "var(--accent)" : "var(--ink-2)"}
          strokeWidth={accent ? 1.25 : 1}
          rx="2"/>
    {/* corner ticks for that schematic feel */}
    {[[0,0],[w,0],[0,h],[w,h]].map(([dx,dy],i)=>(
      <g key={i} transform={`translate(${x+dx} ${y+dy})`} stroke={accent?"var(--accent)":"var(--ink-2)"} strokeWidth="1">
        <line x1={dx===0?0:-6} y1="0" x2={dx===0?6:0} y2="0"/>
        <line x1="0" y1={dy===0?0:-6} x2="0" y2={dy===0?6:0}/>
      </g>
    ))}
    {kbd && (
      <text x={x+w-8} y={y+14} textAnchor="end" fontFamily="var(--font-mono)" fontSize="9"
            fill={accent?"var(--accent)":"var(--ink-3)"} style={{letterSpacing:"0.08em"}}>{kbd}</text>
    )}
    {glyph && (
      <text x={x+10} y={y+18} fontFamily="var(--font-mono)" fontSize="11" fill="var(--accent)" fontWeight="600">{glyph}</text>
    )}
    <text x={x+w/2} y={y+h/2-2} textAnchor="middle" fontFamily="var(--font-mono)" fontSize="13"
          fontWeight="600" fill={accent?"var(--accent)":"var(--ink)"}>{label}</text>
    {sub && (
      <text x={x+w/2} y={y+h/2+15} textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10.5"
            fill="var(--ink-2)">{sub}</text>
    )}
  </g>
);

const Architecture = () => {
  const flow = (d, dur=3, delay=0) => (
    <>
      <path d={d} fill="none" stroke="var(--ink-3)" strokeWidth="1" strokeDasharray="2 4"/>
      <circle r="2.5" fill="var(--accent)">
        <animateMotion dur={`${dur}s`} repeatCount="indefinite" begin={`${delay}s`} path={d}/>
      </circle>
    </>
  );

  return (
    <div className="arch">
      <svg viewBox="0 0 1240 620" className="arch__svg" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <pattern id="grid" width="40" height="40" patternUnits="userSpaceOnUse">
            <path d="M 40 0 L 0 0 0 40" fill="none" stroke="var(--rule)" strokeWidth="0.5"/>
          </pattern>
          <marker id="arr" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--ink-2)"/>
          </marker>
        </defs>
        <rect width="1240" height="620" fill="url(#grid)"/>

        {/* —— Row labels (left gutter) —— */}
        {[
          {y: 30,  t: "01 · CLIENTS"},
          {y: 152, t: "02 · SURFACES"},
          {y: 280, t: "03 · AUTHZ"},
          {y: 360, t: "04 · LOG"},
          {y: 500, t: "05 · VIEWS"},
        ].map((r,i)=>(
          <text key={i} x="12" y={r.y} fontFamily="var(--font-mono)" fontSize="9"
                fill="var(--ink-3)" style={{letterSpacing:"0.12em"}}>{r.t}</text>
        ))}

        {/* —— Clients row —— */}
        {[
          {x: 110, t: "claude desktop", k: "MCP"},
          {x: 290, t: "cursor",         k: "MCP"},
          {x: 470, t: "custom agent",   k: "WIRE"},
          {x: 650, t: "ctxd cli",       k: "DIR"},
          {x: 830, t: "admin ui",       k: "HTTP"},
        ].map((c,i)=>(
          <ArchBox key={i} x={c.x} y={40} w={150} h={52} label={c.t} kbd={c.k}/>
        ))}

        {/* clients → surfaces */}
        {[185, 365, 545, 725, 905].map((x,i)=>(
          <line key={i} x1={x} y1={92} x2={x} y2={140} stroke="var(--ink-3)" strokeWidth="1" markerEnd="url(#arr)"/>
        ))}

        {/* —— Surfaces row —— */}
        <ArchBox x={110} y={140} w={330} h={70} label="MCP server · stdio" sub="5 tools · rmcp" glyph="◆"/>
        <ArchBox x={470} y={140} w={300} h={70} label="wire · :7778" sub="msgpack/tcp · 6 verbs · length-prefixed" glyph="◆"/>
        <ArchBox x={800} y={140} w={255} h={70} label="http admin · :7777" sub="axum · 3 routes" glyph="◆"/>

        {/* surfaces → authz (converging) */}
        <path d="M 275 210 C 275 240, 580 240, 580 270" fill="none" stroke="var(--ink-3)" strokeWidth="1"/>
        <path d="M 620 210 C 620 240, 600 240, 600 270" fill="none" stroke="var(--ink-3)" strokeWidth="1"/>
        <path d="M 925 210 C 925 240, 620 240, 620 270" fill="none" stroke="var(--ink-3)" strokeWidth="1"/>

        {/* —— Capability gate —— */}
        <ArchBox x={110} y={270} w={945} h={48}
          label="capability engine · biscuit-auth · mint  ·  verify  ·  attenuate"
          accent kbd="GATE"/>

        {/* authz → store */}
        <line x1={580} y1={318} x2={580} y2={358} stroke="var(--accent)" strokeWidth="1.25" markerEnd="url(#arr)"/>

        {/* —— Event store (LOG, sequential bricks) —— */}
        <g transform="translate(110 358)">
          <rect width="945" height="100" fill="none" stroke="var(--ink-2)" strokeWidth="1" rx="2"/>
          <text x="14" y="20" fontFamily="var(--font-mono)" fontSize="11" fontWeight="600" fill="var(--ink)">event store · sqlite · append-only</text>
          <text x="14" y="36" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">predecessor-hash chain · CloudEvents v1.0 · UUIDv7 · canonical SHA-256</text>

          {/* event bricks visualizing the log */}
          <g transform="translate(14 50)">
            {Array.from({length:18}).map((_,i)=>(
              <g key={i} transform={`translate(${i*51} 0)`}>
                <rect width="46" height="34" fill="var(--paper-3)" stroke="var(--ink-2)" strokeWidth="0.75"/>
                <text x="23" y="14" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="8" fill="var(--ink-3)">e{String(i).padStart(2,"0")}</text>
                <line x1="6" y1="20" x2="40" y2="20" stroke="var(--accent)" strokeOpacity={0.18+i*0.04} strokeWidth="2"/>
                <text x="23" y="29" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="7" fill="var(--ink-3)">a{i.toString(16)}f{i+3}…</text>
              </g>
            ))}
            {/* hash-chain arrows beneath */}
            {Array.from({length:17}).map((_,i)=>(
              <line key={i} x1={i*51+46} y1={17} x2={(i+1)*51} y2={17} stroke="var(--accent)" strokeOpacity="0.5" strokeWidth="0.75"/>
            ))}
          </g>

          {/* head pointer animation */}
          <g>
            <rect x="14" y="50" width="46" height="34" fill="none" stroke="var(--accent)" strokeWidth="1.5">
              <animate attributeName="x" values="14;65;116;167;218;269;320;371;422;473;524;575;626;677;728;779;830;881" dur="6s" repeatCount="indefinite"/>
            </rect>
            <text x="60" y="98" fontFamily="var(--font-mono)" fontSize="9" fill="var(--accent)">▲ head</text>
          </g>
        </g>

        {/* store → views fan-out */}
        <line x1={250} y1={458} x2={250} y2={500} stroke="var(--ink-3)" strokeWidth="1" markerEnd="url(#arr)"/>
        <line x1={510} y1={458} x2={510} y2={500} stroke="var(--ink-3)" strokeWidth="1" markerEnd="url(#arr)"/>
        <line x1={770} y1={458} x2={770} y2={500} stroke="var(--ink-3)" strokeWidth="1" markerEnd="url(#arr)"/>

        {/* —— Views —— */}
        <ArchBox x={110} y={500} w={290} h={92} label="KV view"
                 sub="latest value · UPSERT on append"/>
        <ArchBox x={420} y={500} w={200} h={92} label="FTS view"
                 sub="SQLite FTS5"/>
        <ArchBox x={640} y={500} w={290} h={92} label="vector view"
                 sub="HNSW · in-mem · embeddings supplied"/>

        {/* small icon glyphs inside view boxes */}
        <g transform="translate(125 565)">
          {/* KV: key→value rows */}
          {[0,1,2].map(i=>(
            <g key={i} transform={`translate(0 ${i*7})`}>
              <rect width="14" height="4" fill="var(--accent)" opacity="0.5"/>
              <rect x="18" width="38" height="4" fill="var(--ink-2)" opacity="0.5"/>
            </g>
          ))}
        </g>
        <g transform="translate(435 568)" stroke="var(--accent)" strokeWidth="0.75" fill="none">
          {/* FTS: magnifier */}
          <circle cx="6" cy="6" r="5"/><line x1="10" y1="10" x2="15" y2="15"/>
        </g>
        <g transform="translate(655 565)">
          {/* vector: dots cloud */}
          {[[0,0],[10,3],[6,8],[16,1],[20,8],[2,12],[14,11]].map(([x,y],i)=>(
            <circle key={i} cx={x} cy={y} r="1.5" fill="var(--accent)" opacity={0.4+i*0.08}/>
          ))}
        </g>

        {/* —— Adapters sidecar —— */}
        <g>
          <rect x={1075} y={140} width={155} height={452} fill="none" stroke="var(--rule-3)" strokeWidth="1" strokeDasharray="3 4" rx="2"/>
          <text x={1153} y={132} textAnchor="middle" fontFamily="var(--font-mono)" fontSize="9" fill="var(--ink-3)" style={{letterSpacing:"0.12em"}}>06 · ADAPTERS</text>

          {[
            ['fs',     'watcher'],
            ['github', 'stub'],
            ['gmail',  'stub'],
            ['your',   'EventSink trait'],
          ].map(([n,d],i)=>(
            <g key={i} transform={`translate(1090 ${160+i*64})`}>
              <rect width="125" height="48" fill="var(--paper-2)" stroke="var(--ink-2)" strokeWidth="1" rx="2"/>
              <text x="10" y="20" fontFamily="var(--font-mono)" fontSize="12" fontWeight="600" fill="var(--ink)">{n}</text>
              <text x="10" y="35" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">{d}</text>
              {/* indicator dot */}
              <circle cx="115" cy="11" r="2.5" fill={i===0?"var(--accent)":"var(--ink-3)"}>
                {i===0 && <animate attributeName="opacity" values="1;.3;1" dur="1.6s" repeatCount="indefinite"/>}
              </circle>
            </g>
          ))}

          <text x={1090} y={460} fontFamily="var(--font-mono)" fontSize="9" fill="var(--ink-3)" style={{letterSpacing:"0.1em"}}>events  ──▶  log</text>
        </g>

        {/* adapters → store flow */}
        {flow("M 1085 184 C 1020 184, 1020 408, 1055 408", 4, 0)}
        {flow("M 1085 248 C 1020 248, 1020 408, 1055 408", 4, 1)}

        {/* main animated packet flow: client → surface → gate → log */}
        {flow("M 185 92 L 185 140 L 275 140 L 275 270 L 580 270 L 580 358", 3.2, 0.0)}
        {flow("M 545 92 L 545 140 L 620 140 L 620 270 L 580 270 L 580 358", 3.2, 1.1)}
        {flow("M 905 92 L 905 140 L 925 140 L 925 270 L 620 270 L 600 270 L 580 270 L 580 358", 3.6, 2.2)}

        {/* legend */}
        <g transform="translate(110 600)">
          <circle r="2.5" fill="var(--accent)"/>
          <text x="10" y="3" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">request packet</text>
          <line x1="140" y1="0" x2="170" y2="0" stroke="var(--accent)" strokeWidth="1.25"/>
          <text x="178" y="3" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">authorized path</text>
          <line x1="320" y1="0" x2="350" y2="0" stroke="var(--ink-3)" strokeDasharray="2 3" strokeWidth="1"/>
          <text x="358" y="3" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">data flow</text>
        </g>
      </svg>
    </div>
  );
};

/* —— MCP integration block —————————————————————— */
const McpBlock = () => (
  <div className="mcp">
    <div className="mcp__col">
      <div className="label" style={{marginBottom: 12}}>tools registered</div>
      <ul className="mcp__tools">
        {[
          ['ctx_write',     'append an event'],
          ['ctx_read',      'read events at subject'],
          ['ctx_subjects',  'list known subjects'],
          ['ctx_search',    'full-text search across log'],
          ['ctx_subscribe', 'poll events since timestamp'],
        ].map(([n,d]) => (
          <li key={n}>
            <span className="mcp__tool">{n}</span>
            <span className="mcp__sep">·</span>
            <span className="mcp__desc">{d}</span>
          </li>
        ))}
      </ul>
    </div>
    <div className="mcp__col">
      <div className="label" style={{marginBottom: 12}}>claude_desktop_config.json</div>
      <pre className="code">{`{
  "mcpServers": {
    `}<span className="c-key">"ctxd"</span>{`: {
      `}<span className="c-key">"command"</span>{`: `}<span className="c-str">"/usr/local/bin/ctxd"</span>{`,
      `}<span className="c-key">"args"</span>{`: [`}<span className="c-str">"serve"</span>{`, `}<span className="c-str">"--mcp-stdio"</span>{`]
    }
  }
}`}</pre>
    </div>
  </div>
);

/* —— Comparison table —————————————————————————— */
const Compare = () => {
  const rows = [
    ['append-only event log',       true,  false, false, false, false],
    ['subject-path addressing',      true,  false, false, false, true],
    ['materialized KV / FTS / vec',  true,  false, false, true,  false],
    ['MCP-native (5 tools)',         true,  false, false, false, false],
    ['capability tokens',            true,  false, false, false, false],
    ['single binary · sqlite',       true,  true,  false, true,  false],
    ['embedding generation',         false, false, false, true,  false],
    ['agent orchestration',          false, false, true,  false, false],
  ];
  const cols = ['', 'ctxd', 'redis', 'langgraph', 'chromadb', 'nats'];

  return (
    <div className="cmp">
      <table>
        <thead>
          <tr>{cols.map(c => <th key={c} className={c==='ctxd' ? 'cmp__us' : ''}>{c}</th>)}</tr>
        </thead>
        <tbody>
          {rows.map((row, ri) => (
            <tr key={ri}>
              <td className="cmp__feat">{row[0]}</td>
              {row.slice(1).map((v, ci) => (
                <td key={ci} className={cls('cmp__cell', ci===0 && 'cmp__us')}>
                  {v ? <span className="cmp__yes">●</span> : <span className="cmp__no">—</span>}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <div className="cmp__note muted-2 mono">
        <em className="serif" style={{fontStyle:'italic'}}>not</em> a vector db · <em className="serif" style={{fontStyle:'italic'}}>not</em> an agent framework · <em className="serif" style={{fontStyle:'italic'}}>not</em> a knowledge graph
      </div>
    </div>
  );
};

/* —— Capability section —————————————————————— */
const CapabilitySection = () => (
  <div className="cap">
    <div className="cap__viz">
      <svg viewBox="0 0 600 360" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="arr2" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
            <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--ink)"/>
          </marker>
        </defs>

        {/* root */}
        <g>
          <rect x="220" y="20" width="160" height="48" fill="var(--ink)"/>
          <text x="300" y="42" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="12" fontWeight="600" fill="var(--paper)">root key</text>
          <text x="300" y="58" textAnchor="middle" fontFamily="var(--font-mono)" fontSize="10" fill="var(--paper-3)">held by daemon</text>
        </g>

        {/* token A */}
        <line x1="300" y1="68" x2="300" y2="100" stroke="var(--ink)" strokeWidth="1" markerEnd="url(#arr2)"/>
        <text x="312" y="86" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-3)">mint</text>
        <g>
          <rect x="200" y="100" width="200" height="64" fill="var(--paper-2)" stroke="var(--ink)" strokeWidth="1.25"/>
          <text x="210" y="120" fontFamily="var(--font-mono)" fontSize="11" fontWeight="600" fill="var(--ink)">token · A</text>
          <text x="210" y="135" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">subject = /**</text>
          <text x="210" y="148" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">ops = read, write</text>
          <text x="210" y="161" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">expires = never</text>
        </g>

        {/* token B */}
        <line x1="300" y1="164" x2="300" y2="196" stroke="var(--ink)" strokeWidth="1" markerEnd="url(#arr2)"/>
        <text x="312" y="182" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-3)">attenuate</text>
        <g>
          <rect x="170" y="196" width="260" height="64" fill="var(--paper-2)" stroke="var(--ink)" strokeWidth="1.25"/>
          <text x="180" y="216" fontFamily="var(--font-mono)" fontSize="11" fontWeight="600" fill="var(--ink)">token · B</text>
          <text x="180" y="231" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">subject = /work/**</text>
          <text x="180" y="244" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">ops = read</text>
          <text x="180" y="257" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-2)">expires = 24h</text>
        </g>

        {/* token C */}
        <line x1="300" y1="260" x2="300" y2="292" stroke="var(--ink)" strokeWidth="1" markerEnd="url(#arr2)"/>
        <text x="312" y="278" fontFamily="var(--font-mono)" fontSize="10" fill="var(--ink-3)">attenuate</text>
        <g>
          <rect x="140" y="292" width="320" height="60" fill="var(--accent-soft)" stroke="var(--accent)" strokeWidth="1.25"/>
          <text x="150" y="312" fontFamily="var(--font-mono)" fontSize="11" fontWeight="600" fill="var(--accent-ink)">token · C  →  given to agent</text>
          <text x="150" y="327" fontFamily="var(--font-mono)" fontSize="10" fill="var(--accent-ink)">subject = /work/acme/**  ·  ops = read</text>
          <text x="150" y="340" fontFamily="var(--font-mono)" fontSize="10" fill="var(--accent-ink)">expires = 1h  ·  kind = ctx.note</text>
        </g>
      </svg>
    </div>
    <div className="cap__copy">
      <div className="label">05 · authorization</div>
      <h3 className="cap__h">
        <span className="serif" style={{fontStyle:'italic'}}>Each level can</span><br/>
        only narrow scope.<br/>
        <span className="muted">Never widen.</span>
      </h3>
      <p className="cap__p">
        ctxd uses biscuit-auth tokens. Every request — MCP call, wire verb, HTTP
        admin — is gated by a token whose caveats are checked against the
        operation. Tokens can be re-attenuated by the holder, so you can hand a
        sub-agent a strictly smaller capability without ever talking to the
        daemon.
      </p>
      <ul className="cap__list">
        <li><span className="cap__tag">SubjectMatches</span> glob pattern restricting paths</li>
        <li><span className="cap__tag">OperationAllowed</span> read · write · subjects · search · admin</li>
        <li><span className="cap__tag">ExpiresAt</span> token invalid past timestamp</li>
        <li><span className="cap__tag">KindAllowed</span> restrict to specific event types</li>
        <li><span className="cap__tag">RateLimit</span> ops/sec cap</li>
      </ul>
      <p className="cap__p muted">
        Verification is datalog-injection-safe. All user inputs are validated
        before interpolation into biscuit authorizer code.
      </p>
    </div>
  </div>
);

Object.assign(window, {
  TerminalDemo,
  Architecture,
  McpBlock,
  Compare,
  CapabilitySection,
});
