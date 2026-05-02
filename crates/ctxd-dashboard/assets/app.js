// ctxd dashboard — vanilla JS frontend.
// No frameworks, no build step. Hash routing, per-view fetch on render,
// SSE live tail on overview only.

(function () {
  'use strict';

  // ───────── Utilities ─────────

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

  function el(tag, props = {}, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(props)) {
      if (k === 'class') node.className = v;
      else if (k === 'html') node.innerHTML = v;
      else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
      else if (v !== undefined && v !== null) node.setAttribute(k, v);
    }
    for (const child of children) {
      if (child == null) continue;
      node.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return node;
  }

  function fmtTime(iso) {
    if (!iso) return '';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
  }

  function fmtCount(n) {
    if (n == null) return '—';
    if (n < 1000) return String(n);
    if (n < 1_000_000) return (n / 1000).toFixed(n < 10000 ? 1 : 0) + 'k';
    return (n / 1_000_000).toFixed(1) + 'M';
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]
    );
  }

  // The server already wraps matched terms in <mark>; trust those tags
  // and escape everything else. Implemented as a small state machine
  // so we don't pull in a full HTML parser.
  function renderSnippet(s) {
    if (!s) return '';
    const parts = s.split(/(<\/?mark>)/i);
    let out = '';
    for (const p of parts) {
      if (p.toLowerCase() === '<mark>') out += '<mark>';
      else if (p.toLowerCase() === '</mark>') out += '</mark>';
      else out += escapeHtml(p);
    }
    return out;
  }

  async function api(path, opts = {}) {
    const resp = await fetch(path, opts);
    if (!resp.ok) {
      const text = await resp.text().catch(() => '');
      const err = new Error(`${resp.status} ${resp.statusText}: ${text || path}`);
      err.status = resp.status;
      throw err;
    }
    return resp.json();
  }

  // ───────── Live badge ─────────

  const liveBadge = $('#live-badge');
  function setLive(state) {
    liveBadge.classList.remove('live-off', 'live-on', 'live-reconnecting');
    liveBadge.classList.add(`live-${state}`);
    const text = { off: 'offline', on: 'live', reconnecting: 'reconnecting…' }[state] || state;
    $('.live-text', liveBadge).textContent = text;
  }
  setLive('off');

  // ───────── Router ─────────

  const routes = {
    overview: renderOverview,
    subjects: renderSubjects,
    search: renderSearch,
    peers: renderPeers,
    event: renderEvent,
    subject: renderSubjectDetail,
  };

  let currentRoute = null;
  let currentCleanup = null;

  function parseRoute() {
    const hash = location.hash.replace(/^#\/?/, '') || '';
    const [path, query] = hash.split('?');
    const segments = path.split('/').filter(Boolean);
    const route = segments[0] || 'overview';
    const rest = segments.slice(1);
    const params = new URLSearchParams(query || '');
    return { route, rest, params };
  }

  function navigate() {
    const { route, rest, params } = parseRoute();
    if (currentCleanup) {
      currentCleanup();
      currentCleanup = null;
    }
    const renderer = routes[route] || routes.overview;
    currentRoute = route;
    // Update nav highlight.
    $$('.hdr-nav a').forEach((a) => {
      a.classList.toggle('active', a.dataset.route === route);
    });
    const main = $('#main');
    main.innerHTML = '<p class="loading">loading…</p>';
    Promise.resolve(renderer(main, params, rest)).catch((err) => {
      main.innerHTML = '';
      main.append(errorPanel(`failed to render ${route}`, err));
    });
  }

  window.addEventListener('hashchange', navigate);

  $('#refresh-btn').addEventListener('click', navigate);

  // ───────── Overview ─────────

  let overviewSse = null;
  let overviewSseRetries = 0;

  async function renderOverview(main) {
    // Layout shell first so the user sees structure immediately.
    main.innerHTML = '';
    const statsStrip = el('dl', { class: 'stats' });
    const recentPanel = panel('Recent events', el('div', { class: 'events', id: 'recent-events' },
      ...shimmerRows(5)));
    const subjectsPanel = panel('Top subjects', el('div', { class: 'tree', id: 'top-subjects' },
      ...shimmerRows(5)));

    main.append(statsStrip);
    const cols = el('div', { class: 'two-col' });
    cols.append(subjectsPanel, recentPanel);
    main.append(cols);

    let cleanedUp = false;
    currentCleanup = () => {
      cleanedUp = true;
      stopOverviewSse();
    };

    const stats = await api('/v1/stats').catch(() => null);
    if (cleanedUp) return;
    if (!stats) {
      statsStrip.replaceWith(errorPanel('couldn\'t load stats', new Error('stats unavailable')));
      return;
    }
    renderStats(statsStrip, stats);

    if (stats.event_count === 0) {
      // Empty state: replace recent-events with the hello-world tutorial.
      $('#recent-events').replaceWith(emptyStateTutorial());
      $('#top-subjects').replaceWith(el('div', { class: 'empty' },
        el('h3', {}, 'no subjects yet'),
        el('p', {}, 'as you write events to your substrate, the tree of subject paths shows up here.')));
      return;
    }

    // Recent events + subjects loaded in parallel.
    Promise.all([
      api('/v1/events?limit=50').catch(() => null),
      api('/v1/subjects/tree?prefix=/').catch(() => null),
    ]).then(([events, tree]) => {
      if (cleanedUp) return;
      if (events && Array.isArray(events.events)) renderRecentEvents($('#recent-events'), events.events);
      if (tree) renderTopSubjects($('#top-subjects'), tree);
    });

    startOverviewSse();
  }

  function renderStats(target, s) {
    const items = [
      { k: 'events', v: s.event_count, accent: true },
      { k: 'subjects', v: s.subject_count },
      { k: 'peers', v: s.peer_count },
      { k: 'pending', v: s.pending_approval_count },
      { k: 'embeddings', v: s.vector_embedding_count },
      { k: 'uptime', v: fmtUptime(s.uptime_seconds), raw: true },
    ];
    target.innerHTML = '';
    for (const it of items) {
      target.append(el('div', { class: 'stat' + (it.accent ? ' stat--accent' : '') },
        el('dt', {}, it.k),
        el('dd', {}, it.raw ? it.v : fmtCount(it.v))));
    }
  }

  function fmtUptime(seconds) {
    if (seconds == null) return '—';
    const m = Math.floor(seconds / 60);
    const h = Math.floor(seconds / 3600);
    const d = Math.floor(seconds / 86400);
    if (d > 0) return `${d}d ${h % 24}h`;
    if (h > 0) return `${h}h ${m % 60}m`;
    if (m > 0) return `${m}m ${seconds % 60}s`;
    return `${seconds}s`;
  }

  function renderRecentEvents(target, events) {
    target.innerHTML = '';
    if (!events.length) {
      target.append(el('p', { class: 'muted' }, 'no events.'));
      return;
    }
    for (const e of events) target.append(eventRow(e, false));
  }

  function eventRow(e, fresh) {
    return el('a', { class: 'event-row', href: `#/event/${e.id}` },
      el('span', { class: 'ts' + (fresh ? ' fresh' : '') }, fmtTime(e.time)),
      el('span', { class: 'ty' }, e.type || ''),
      el('span', { class: 'sub' }, e.subject));
  }

  function prependEventRow(e) {
    const target = $('#recent-events');
    if (!target) return;
    const row = eventRow(e, true);
    target.prepend(row);
    // Cap at ~100 to stop runaway memory.
    while (target.children.length > 100) target.lastChild.remove();
  }

  function renderTopSubjects(target, tree) {
    target.innerHTML = '';
    // Show top-level children sorted by count desc, capped to 10.
    const kids = (tree.children || []).slice().sort((a, b) => b.count - a.count).slice(0, 10);
    if (!kids.length) {
      target.append(el('p', { class: 'muted' }, 'no subjects.'));
      return;
    }
    const list = el('ul');
    for (const c of kids) {
      const li = el('li');
      li.append(el('span', { class: 'tree-node' },
        el('span', { class: 'nm' }, c.name),
        el('span', { class: 'ct' }, fmtCount(c.count))));
      list.append(li);
    }
    target.append(list);
  }

  function emptyStateTutorial() {
    const wrap = el('div', { class: 'empty', id: 'empty-tutorial' },
      el('h3', {}, 'no events yet'),
      el('p', {}, 'your substrate is empty. write a hello-world event to see it work.'),
      el('button', { class: 'btn btn--primary', type: 'button' }, 'write a hello-world event'));
    wrap.querySelector('button').addEventListener('click', async (ev) => {
      ev.target.disabled = true;
      ev.target.textContent = 'writing…';
      try {
        await fetch('/v1/dashboard/hello-world', { method: 'POST' });
        // Re-render the overview so stats and the events panel light up.
        navigate();
      } catch (e) {
        ev.target.textContent = 'failed — try again';
        ev.target.disabled = false;
      }
    });
    return wrap;
  }

  // ───────── SSE live tail ─────────

  function startOverviewSse() {
    if (document.visibilityState === 'hidden') return;
    overviewSse = new EventSource('/v1/events/stream');
    setLive('reconnecting');
    overviewSse.onopen = () => {
      setLive('on');
      overviewSseRetries = 0;
    };
    overviewSse.addEventListener('event', (msg) => {
      try {
        const ev = JSON.parse(msg.data);
        prependEventRow(ev);
        // Bump the event count.
        const dd = $('.stat--accent dd');
        if (dd) {
          const prev = parseInt(dd.textContent.replace(/\D+/g, ''), 10) || 0;
          dd.textContent = fmtCount(prev + 1);
        }
      } catch (_) { /* swallow */ }
    });
    overviewSse.addEventListener('lagged', () => {
      // Treat a lagged event as a hint to refresh the recent slice.
      api('/v1/events?limit=50').then((j) => {
        if (j && Array.isArray(j.events)) renderRecentEvents($('#recent-events'), j.events);
      }).catch(() => {});
    });
    overviewSse.onerror = () => {
      setLive('reconnecting');
      // EventSource auto-reconnects with exponential-ish backoff; we
      // just need to display the right status. Cap retries to avoid
      // a flapping badge if the backend is permanently down.
      overviewSseRetries++;
      if (overviewSseRetries > 6) setLive('off');
    };
  }

  function stopOverviewSse() {
    if (overviewSse) {
      overviewSse.close();
      overviewSse = null;
    }
    setLive('off');
  }

  document.addEventListener('visibilitychange', () => {
    if (currentRoute !== 'overview') return;
    if (document.visibilityState === 'hidden') stopOverviewSse();
    else if (!overviewSse) startOverviewSse();
  });

  // ───────── Subjects view ─────────

  async function renderSubjects(main, params) {
    main.innerHTML = '';
    const prefix = params.get('prefix') || '/';
    const head = el('div', { class: 'panel-head' },
      el('h2', {}, `subjects ${prefix === '/' ? '' : '· ' + prefix}`));
    const treeContainer = el('div', { class: 'tree' }, ...shimmerRows(8));
    main.append(panel(null, head, treeContainer));

    const tree = await api(`/v1/subjects/tree?prefix=${encodeURIComponent(prefix)}`).catch((e) => {
      treeContainer.replaceWith(errorPanel('couldn\'t load tree', e));
      return null;
    });
    if (!tree) return;
    treeContainer.innerHTML = '';
    if (!tree.children || !tree.children.length) {
      treeContainer.append(el('p', { class: 'muted' }, 'no subjects under this prefix.'));
      return;
    }
    treeContainer.append(buildTreeNode(tree));
  }

  function buildTreeNode(node, depth = 0) {
    const ul = el('ul');
    for (const child of (node.children || [])) {
      const li = el('li');
      const expandable = (child.children || []).length > 0;

      // Chevron toggles expand/collapse; the name is a real link to
      // the subject's events. Two distinct click targets so the user
      // can drill into either dimension without "click the name to
      // expand, click the name *again* to view events" ambiguity.
      const chev = el('span', {
        class: 'chev' + (expandable ? ' chev--btn' : ''),
        role: expandable ? 'button' : null,
        'aria-label': expandable ? 'toggle children' : null,
      }, expandable ? '▸' : ' ');

      const nameLink = el('a', {
        class: 'nm',
        href: `#/subject/${encodeURIComponent(child.name)}`,
        title: `view events under ${child.name}`,
      }, child.name);

      const nodeLine = el('span', { class: 'tree-node', role: 'treeitem' },
        chev,
        nameLink,
        el('span', { class: 'ct' }, fmtCount(child.count)));
      li.append(nodeLine);

      if (expandable) {
        let expanded = depth < 1;
        const subUl = expanded ? buildTreeNode(child, depth + 1) : null;
        if (subUl) li.append(subUl);
        chev.addEventListener('click', (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          expanded = !expanded;
          chev.textContent = expanded ? '▾' : '▸';
          if (expanded) {
            li.append(buildTreeNode(child, depth + 1));
          } else {
            li.querySelector('ul')?.remove();
          }
        });
        if (subUl) chev.textContent = '▾';
      }
      ul.append(li);
    }
    return ul;
  }

  // ───────── Subject detail view ─────────
  //
  // Renders events under a given subject (recursive). Hash route:
  // #/subject/<url-encoded-path>. Reached by clicking a subject name
  // in the tree, or by typing the URL.
  //
  // The subject path is URL-encoded as a single segment so paths like
  // `/work/local/files/a.md` survive routing without the slashes
  // confusing parseRoute.

  async function renderSubjectDetail(main, _params, rest) {
    main.innerHTML = '';
    const subject = rest.length ? decodeURIComponent(rest[0]) : null;
    if (!subject) {
      main.append(errorPanel('no subject', new Error('expected #/subject/<path>')));
      return;
    }

    const back = el('a', { class: 'event-back', href: '#/subjects' }, '← all subjects');
    main.append(back);

    // Header panel: subject path + count.
    const head = el('div', { class: 'panel-head' },
      el('h2', {}, subject));
    const headPanel = el('section', { class: 'panel' }, head);
    const eventsHost = el('div', { class: 'events', id: 'subject-events' },
      ...shimmerRows(8));
    headPanel.append(eventsHost);
    main.append(headPanel);

    // Pull events under this subject (recursive). The endpoint's
    // subject filter implies recursive descent.
    let page;
    try {
      page = await api(`/v1/events?subject=${encodeURIComponent(subject)}&limit=200`);
    } catch (e) {
      eventsHost.replaceWith(errorPanel('couldn\'t load events', e));
      return;
    }

    // Update the head with the count once we know it.
    head.append(el('span', { class: 'panel-meta' }, `${page.events.length} event${page.events.length === 1 ? '' : 's'}`));

    eventsHost.innerHTML = '';
    if (!page.events.length) {
      eventsHost.append(emptyMsg(
        `no events under ${subject}`,
        'this subject is empty or has only descendants. try the parent prefix to see them.'));
      return;
    }
    for (const e of page.events) eventsHost.append(eventRow(e, false));

    // If there's a next page, surface a "load older" button. The
    // cursor is an API implementation detail (opaque base64 of seq);
    // we don't show it in the UI.
    if (page.next_cursor) {
      const more = el('button', { class: 'btn btn--ghost btn--sm', type: 'button' },
        'load older');
      let cursor = page.next_cursor;
      more.addEventListener('click', async () => {
        more.disabled = true;
        more.textContent = 'loading…';
        try {
          const next = await api(
            `/v1/events?subject=${encodeURIComponent(subject)}&before=${encodeURIComponent(cursor)}&limit=200`);
          for (const e of next.events) eventsHost.append(eventRow(e, false));
          if (next.next_cursor) {
            cursor = next.next_cursor;
            more.disabled = false;
            more.textContent = 'load older';
          } else {
            more.remove();
          }
        } catch (e) {
          more.textContent = 'failed — click to retry';
          more.disabled = false;
        }
      });
      headPanel.append(more);
    }
  }

  // ───────── Search view ─────────

  let searchTimer = null;

  async function renderSearch(main, params) {
    main.innerHTML = '';
    const initialQ = params.get('q') || '';
    const input = el('input', {
      class: 'search-box',
      type: 'search',
      placeholder: 'search events…',
      value: initialQ,
      'aria-label': 'search events',
    });
    const meta = el('div', { class: 'search-meta' });
    const resultsHost = el('div', { class: 'search-results' });
    main.append(input, meta, resultsHost);
    input.focus();

    async function run(q) {
      if (!q) {
        meta.textContent = '';
        resultsHost.innerHTML = '';
        return;
      }
      meta.textContent = 'searching…';
      try {
        const r = await api(`/v1/search?q=${encodeURIComponent(q)}`);
        meta.textContent = `${r.results.length} result${r.results.length === 1 ? '' : 's'} · ${r.took_ms}ms`;
        resultsHost.innerHTML = '';
        if (!r.results.length) {
          resultsHost.append(el('div', { class: 'empty' },
            el('h3', {}, 'no matches'),
            el('p', {}, `nothing matched "${q}". try a broader term.`)));
          return;
        }
        for (const hit of r.results) {
          resultsHost.append(searchResult(hit));
        }
      } catch (e) {
        resultsHost.innerHTML = '';
        resultsHost.append(errorPanel('search failed', e));
      }
    }

    input.addEventListener('input', () => {
      clearTimeout(searchTimer);
      const q = input.value.trim();
      // Reflect query in the URL so a refresh keeps the search.
      const newHash = q ? `#/search?q=${encodeURIComponent(q)}` : '#/search';
      if (location.hash !== newHash) history.replaceState(null, '', newHash);
      searchTimer = setTimeout(() => run(q), 150);
    });

    if (initialQ) run(initialQ);
  }

  function searchResult(hit) {
    return el('a', { class: 'search-result', href: `#/event/${hit.id}` },
      el('div', { class: 'head' },
        el('span', { class: 'ts' }, fmtTime(hit.time)),
        el('span', { class: 'ty' }, hit.type || ''),
        el('span', { class: 'sub' }, hit.subject)),
      el('div', { class: 'snippet', html: renderSnippet(hit.snippet) }));
  }

  // ───────── Event detail view ─────────
  //
  // Renders /v1/events/<id>. Lead with the actual content (the
  // memory you wrote), then metadata + signature/parents/attestation
  // for the technically curious. Hash route: #/event/<id>.

  async function renderEvent(main, _params, rest) {
    main.innerHTML = '';
    const id = rest[0];
    if (!id) {
      main.append(errorPanel('no event id', new Error('expected #/event/<id>')));
      return;
    }

    const back = el('a', { class: 'event-back', href: 'javascript:history.back()' },
      '← back');
    main.append(back);

    let event;
    try {
      event = await api(`/v1/events/${encodeURIComponent(id)}`);
    } catch (e) {
      if (e.status === 404) {
        main.append(emptyMsg('event not found',
          `no event with id ${id}. it may have been from a different database, or the id may be malformed.`));
      } else {
        main.append(errorPanel('couldn\'t load event', e));
      }
      return;
    }

    // Headline: the content. data.content for ctx.note / ctx.fact /
    // etc.; falls back to pretty-printed JSON for events that don't
    // follow that convention.
    const contentHost = el('div', { class: 'event-content' });
    const content = event.data && typeof event.data === 'object'
      ? event.data.content
      : null;
    if (typeof content === 'string' && content.length > 0) {
      contentHost.append(el('pre', { class: 'event-text' }, content));
    } else {
      // No `content` field — show the whole `data` blob.
      contentHost.append(el('pre', { class: 'event-json' }, prettyJson(event.data)));
    }
    main.append(panel('Content', contentHost));

    // Metadata table (subject, type, time, source).
    const meta = el('dl', { class: 'event-meta' });
    addMetaRow(meta, 'subject', event.subject);
    addMetaRow(meta, 'type', event.type || event.event_type);
    addMetaRow(meta, 'time', `${event.time} (${fmtTime(event.time)})`);
    if (event.source) addMetaRow(meta, 'source', event.source);
    addMetaRow(meta, 'id', event.id);
    main.append(panel('Metadata', meta));

    // Provenance: predecessor hash, parents, signature, attestation.
    // Most events have these blank or null; only render the section
    // if at least one is set so we don't waste vertical space.
    const provHost = el('dl', { class: 'event-meta' });
    let hasProv = false;
    if (event.predecessorhash) { addMetaRow(provHost, 'predecessor', event.predecessorhash); hasProv = true; }
    if (event.parents && event.parents.length) { addMetaRow(provHost, 'parents', event.parents.join(', ')); hasProv = true; }
    if (event.signature) { addMetaRow(provHost, 'signature', event.signature); hasProv = true; }
    if (event.attestation) { addMetaRow(provHost, 'attestation', `${event.attestation.length} bytes`); hasProv = true; }
    if (hasProv) main.append(panel('Provenance', provHost));

    // Raw JSON, collapsed by default — for the "I trust nothing,
    // show me the wire form" power user.
    const detail = el('details', { class: 'event-raw' },
      el('summary', {}, 'raw event JSON'),
      el('pre', { class: 'event-json' }, prettyJson(event)));
    main.append(panel('Raw', detail));
  }

  function addMetaRow(dl, label, value) {
    dl.append(el('dt', {}, label));
    dl.append(el('dd', {}, String(value == null ? '' : value)));
  }

  function prettyJson(v) {
    try { return JSON.stringify(v, null, 2); }
    catch (_) { return String(v); }
  }

  // ───────── Peers view ─────————

  async function renderPeers(main) {
    main.innerHTML = '';
    main.append(panel('Peers', el('div', { class: 'peer-list', id: 'peer-list' }, ...shimmerRows(3))));
    main.append(panel('Pending approvals', el('div', { class: 'approval-list', id: 'approval-list' },
      ...shimmerRows(2))));

    Promise.all([
      api('/v1/peers').catch((e) => ({ error: e })),
      api('/v1/approvals').catch((e) => ({ error: e })),
    ]).then(([peers, approvals]) => {
      const peerHost = $('#peer-list');
      peerHost.innerHTML = '';
      if (peers.error) {
        if (peers.error.status === 401 || peers.error.status === 403) {
          peerHost.append(emptyMsg('peers require an admin token',
            'this view needs admin auth, which the dashboard\'s loopback bypass doesn\'t grant. set up a token via `ctxd grant --operations admin`.'));
        } else {
          peerHost.append(errorPanel('peers unavailable', peers.error));
        }
      } else if (!peers.peers || !peers.peers.length) {
        peerHost.append(emptyMsg('no peers',
          'federation is not configured. peer your daemon with `ctxd peer add` to see them here.'));
      } else {
        for (const p of peers.peers) {
          peerHost.append(el('div', { class: 'peer-row' },
            el('span', { class: 'id' }, p.peer_id),
            el('span', { class: 'url' }, p.url + ' · ' + (p.subject_patterns || []).join(', '))));
        }
      }

      const approvalHost = $('#approval-list');
      approvalHost.innerHTML = '';
      const pending = (approvals && approvals.pending) || [];
      if (approvals && approvals.error) {
        approvalHost.append(errorPanel('approvals unavailable', approvals.error));
      } else if (!pending.length) {
        approvalHost.append(emptyMsg('no pending approvals', 'when a caveat needs human approval, it shows up here.'));
      } else {
        for (const a of pending) {
          approvalHost.append(el('div', { class: 'approval-row' },
            el('span', { class: 'id' }, a.approval_id || a.id || ''),
            el('span', { class: 'meta' }, `${a.operation || ''} · ${a.subject || ''}`)));
        }
      }
    });
  }

  // ───────── Helpers ─────────

  function panel(title, ...children) {
    const p = el('section', { class: 'panel' });
    if (title) {
      p.append(el('div', { class: 'panel-head' }, el('h2', {}, title)));
    }
    for (const c of children) p.append(c);
    return p;
  }

  function shimmerRows(n) {
    const out = [];
    for (let i = 0; i < n; i++) out.push(el('div', { class: 'shimmer' }));
    return out;
  }

  function emptyMsg(title, msg) {
    return el('div', { class: 'empty' },
      el('h3', {}, title),
      el('p', {}, msg));
  }

  function errorPanel(title, err) {
    const msg = err instanceof Error ? err.message : String(err);
    return el('div', { class: 'error' },
      el('h3', {}, title),
      el('p', {}, msg));
  }

  // Boot.
  if (!location.hash) location.hash = '#/';
  navigate();
})();
