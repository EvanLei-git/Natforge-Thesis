/* NatForge - shared UI helpers: line-SVG icons, auth guards, tabs, modal, toast.
   No external framework, no emoji. */

// ---- Line-SVG icon set (stroke, 24x24, currentColor) ----
const NF_ICONS = {
    service: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><line x1="7" y1="7.5" x2="7.01" y2="7.5"/><line x1="7" y1="16.5" x2="7.01" y2="16.5"/></svg>',
    admin: '<svg viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6l7-3z"/><path d="M9 12l2 2 4-4"/></svg>',
    logout: '<svg viewBox="0 0 24 24"><path d="M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3"/><path d="M10 17l-5-5 5-5"/><line x1="5" y1="12" x2="16" y2="12"/></svg>',
    plus: '<svg viewBox="0 0 24 24"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>',
    close: '<svg viewBox="0 0 24 24"><line x1="6" y1="6" x2="18" y2="18"/><line x1="18" y1="6" x2="6" y2="18"/></svg>',
    chevron: '<svg viewBox="0 0 24 24"><polyline points="6 9 12 15 18 9"/></svg>',
    stop: '<svg viewBox="0 0 24 24"><rect x="6" y="6" width="12" height="12" rx="1.5"/></svg>',
    link: '<svg viewBox="0 0 24 24"><path d="M14 5h5v5"/><line x1="19" y1="5" x2="10" y2="14"/><path d="M19 13v5a1.5 1.5 0 0 1-1.5 1.5H6.5A1.5 1.5 0 0 1 5 18V6.5A1.5 1.5 0 0 1 6.5 5H11"/></svg>',
    device: '<svg viewBox="0 0 24 24"><rect x="7" y="7" width="10" height="10" rx="1.5"/><line x1="10" y1="3" x2="10" y2="6"/><line x1="14" y1="3" x2="14" y2="6"/><line x1="10" y1="18" x2="10" y2="21"/><line x1="14" y1="18" x2="14" y2="21"/><line x1="3" y1="10" x2="6" y2="10"/><line x1="3" y1="14" x2="6" y2="14"/><line x1="18" y1="10" x2="21" y2="10"/><line x1="18" y1="14" x2="21" y2="14"/></svg>',
    bolt: '<svg viewBox="0 0 24 24"><path d="M13 2L4 14h7l-1 8 9-12h-7l1-8z"/></svg>',
    globe: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3c2.6 2.6 2.6 15.4 0 18M12 3c-2.6 2.6-2.6 15.4 0 18"/></svg>',
    shield: '<svg viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6l7-3z"/></svg>',
    server: '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><line x1="7" y1="7.5" x2="7.01" y2="7.5"/><line x1="7" y1="16.5" x2="7.01" y2="16.5"/></svg>',
    user: '<svg viewBox="0 0 24 24"><circle cx="12" cy="8" r="3.5"/><path d="M5.5 20a6.5 6.5 0 0 1 13 0"/></svg>',
    trash: '<svg viewBox="0 0 24 24"><path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6 7l1 13a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-13"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/></svg>',
    edit: '<svg viewBox="0 0 24 24"><path d="M4 20h4L18 10l-4-4L4 16v4z"/><line x1="13" y1="6" x2="17" y2="10"/></svg>',
    key: '<svg viewBox="0 0 24 24"><circle cx="8" cy="12" r="4"/><line x1="11" y1="12" x2="21" y2="12"/><line x1="18" y1="12" x2="18" y2="15"/><line x1="21" y1="12" x2="21" y2="16"/></svg>',
    activity: '<svg viewBox="0 0 24 24"><path d="M3 12h4l3 7 4-14 3 7h4"/></svg>',
    ban: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><line x1="5.6" y1="5.6" x2="18.4" y2="18.4"/></svg>',
    users: '<svg viewBox="0 0 24 24"><circle cx="9" cy="8" r="3.2"/><path d="M3.5 19a5.5 5.5 0 0 1 11 0"/><path d="M16 5.2a3.2 3.2 0 0 1 0 6.1"/><path d="M17.5 13.5a5.5 5.5 0 0 1 3 5.5"/></svg>',
    upload: '<svg viewBox="0 0 24 24"><path d="M12 16V4"/><path d="M7 9l5-5 5 5"/><path d="M5 20h14"/></svg>',
    download: '<svg viewBox="0 0 24 24"><path d="M12 4v12"/><path d="M7 11l5 5 5-5"/><path d="M5 20h14"/></svg>',
};

function nfRenderIcons(root) {
    (root || document).querySelectorAll('[data-icon]').forEach(el => {
        const n = el.getAttribute('data-icon');
        if (NF_ICONS[n] && !el.dataset.iconDone) { el.classList.add('icon'); el.innerHTML = NF_ICONS[n]; el.dataset.iconDone = '1'; }
    });
}
function nfIcon(name) { return `<span class="icon" data-icon-done="1">${NF_ICONS[name] || ''}</span>`; }

// ---- Auth guards ----
function requireAuth(adminOnly = false) {
    if (!window.API || !window.API.isAuthed()) { window.location.href = '/signin'; return false; }
    if (adminOnly && window.API.role !== 'admin') { toast('Administrator role required', 'danger'); setTimeout(() => location.href = '/dashboard', 1200); return false; }
    return true;
}
function logout() { window.API.clearSession(); window.location.href = '/signin'; }
function applyRoleVisibility() {
    if (!window.API) return;
    if (window.API.role !== 'admin')
        document.querySelectorAll('.admin-only').forEach(el => el.style.display = 'none');
    else
        document.querySelectorAll('.user-only').forEach(el => el.style.display = 'none');
}

function fmtBytes(n) {
    if (!n) return '0 B';
    const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0, v = Number(n);
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(i === 0 ? 0 : 1)} ${u[i]}`;
}

// Escape text for safe interpolation into innerHTML (user-controlled strings).
function escapeHtml(s) {
    return String(s == null ? '' : s).replace(/[&<>"']/g, c =>
        ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

// Escape a value for an inline event-handler ATTRIBUTE that is also JS,
// e.g. onclick="fn(${escapeAttr(x)})". JSON-encode first so the value is a
// valid JS literal (strings get quotes, " and \ escaped), then HTML-encode so
// no ' " < > can terminate the attribute during HTML parsing - the browser
// HTML-decodes it back before the JS engine sees it. Without this, a tunnel
// name or email containing a quote would break out of the handler (stored XSS).
function escapeAttr(v) {
    return escapeHtml(JSON.stringify(v == null ? '' : v));
}

// Relative "time ago" from an ISO timestamp (for last-seen columns).
function fmtAgo(iso) {
    if (!iso) return '-';
    const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86400)}d ago`;
}

// Paint a range input's filled portion in the brand colour (cross-browser).
function nfSlider(el) {
    if (!el) return;
    const min = +el.min || 0, max = +el.max || 100;
    const pct = ((+el.value - min) / (max - min)) * 100;
    el.style.background = `linear-gradient(to right, var(--brand) 0 ${pct}%, var(--bg-raised) ${pct}% 100%)`;
}
// Auto-wire any range input with class "nf-range".
document.addEventListener('DOMContentLoaded', () => {
    document.querySelectorAll('input[type=range].nf-range').forEach(el => {
        nfSlider(el);
        el.addEventListener('input', () => nfSlider(el));
    });
});

// ---- Tabs ----
function nfTabs() {
    document.querySelectorAll('[data-tab]').forEach(btn => {
        btn.addEventListener('click', () => {
            const group = btn.closest('[data-tabs]');
            const target = btn.getAttribute('data-tab');
            group.querySelectorAll('[data-tab]').forEach(b => b.classList.toggle('active', b === btn));
            document.querySelectorAll(`[data-panel]`).forEach(p => {
                if (p.closest('[data-tabs]') === group || p.dataset.group === group.dataset.tabs)
                    p.classList.toggle('active', p.getAttribute('data-panel') === target);
            });
        });
    });
}

// ---- Modal ----
function nfOpenModal(id) { const m = document.getElementById(id); if (m) m.classList.add('open'); }
function nfCloseModal(id) { const m = document.getElementById(id); if (m) m.classList.remove('open'); }
function nfWireModals() {
    document.querySelectorAll('[data-modal-open]').forEach(b => b.addEventListener('click', () => nfOpenModal(b.getAttribute('data-modal-open'))));
    document.querySelectorAll('[data-modal-close]').forEach(b => b.addEventListener('click', () => b.closest('.modal-overlay').classList.remove('open')));
    document.querySelectorAll('.modal-overlay').forEach(o => o.addEventListener('click', e => { if (e.target === o) o.classList.remove('open'); }));
}

// ---- Promise-based confirm / prompt (design-system modals; replace native confirm()/prompt()) ----
function nfConfirm({ title = 'Confirm', message = '', confirmText = 'Confirm', cancelText = 'Cancel', danger = false } = {}) {
    return new Promise(resolve => {
        const overlay = document.createElement('div');
        overlay.className = 'modal-overlay open';
        overlay.innerHTML =
            `<div class="modal" style="max-width:440px">
               <div class="modal-head"><h5>${escapeHtml(title)}</h5>
                 <button class="x-btn" data-nf="cancel" aria-label="Close">${nfIcon('close')}</button></div>
               <div class="modal-body">
                 <p class="muted" style="margin-top:0">${escapeHtml(message)}</p>
                 <div class="row-inline" style="justify-content:flex-end; gap:8px; margin-top:16px">
                   <button class="btn btn-secondary" data-nf="cancel">${escapeHtml(cancelText)}</button>
                   <button class="btn ${danger ? 'btn-danger' : 'btn-primary'}" data-nf="ok">${escapeHtml(confirmText)}</button>
                 </div>
               </div>
             </div>`;
        const done = v => { overlay.remove(); document.removeEventListener('keydown', onKey); resolve(v); };
        const onKey = e => { if (e.key === 'Escape') done(false); else if (e.key === 'Enter') done(true); };
        overlay.addEventListener('click', e => {
            if (e.target === overlay) return done(false);
            const act = e.target.closest('[data-nf]');
            if (act) done(act.getAttribute('data-nf') === 'ok');
        });
        document.addEventListener('keydown', onKey);
        document.body.appendChild(overlay);
        const ok = overlay.querySelector('[data-nf="ok"]'); if (ok) ok.focus();
    });
}

// fields: [{ key, label, value, placeholder, hint, type, maxlength, pattern }]
// Resolves to an object { key: value, ... } on confirm, or null on cancel.
function nfPrompt({ title = 'Edit', message = '', fields = [], confirmText = 'Save', cancelText = 'Cancel' } = {}) {
    return new Promise(resolve => {
        const overlay = document.createElement('div');
        overlay.className = 'modal-overlay open';
        // Inputs are built STRUCTURALLY here (no user data in the markup); their
        // value / placeholder / pattern / type / key are assigned as DOM *properties*
        // after parsing, so nothing user-controlled is ever interpreted as HTML or
        // an attribute - closing the attribute-injection surface entirely. Only the
        // label/hint text (a safe text context) is interpolated, via escapeHtml.
        const fieldHtml = fields.map(f =>
            `<div class="field">
               <label class="lbl">${escapeHtml(f.label || f.key)}${f.hint ? ` <span class="faint small">${escapeHtml(f.hint)}</span>` : ''}</label>
               ${f.type === 'select' ? '<select class="input nf-field"></select>' : '<input class="input nf-field">'}
             </div>`).join('');
        overlay.innerHTML =
            `<div class="modal" style="max-width:460px">
               <div class="modal-head"><h5>${escapeHtml(title)}</h5>
                 <button class="x-btn" data-nf="cancel" aria-label="Close">${nfIcon('close')}</button></div>
               <div class="modal-body">
                 ${message ? `<p class="muted" style="margin-top:0">${escapeHtml(message)}</p>` : ''}
                 ${fieldHtml}
                 <div class="row-inline" style="justify-content:flex-end; gap:8px; margin-top:8px">
                   <button class="btn btn-secondary" data-nf="cancel">${escapeHtml(cancelText)}</button>
                   <button class="btn btn-primary" data-nf="ok">${escapeHtml(confirmText)}</button>
                 </div>
               </div>
             </div>`;
        const els = Array.from(overlay.querySelectorAll('.nf-field'));
        els.forEach((inp, i) => {
            const f = fields[i];
            inp.dataset.key = f.key;
            if (f.type === 'select') {
                (f.options || []).forEach(o => {
                    const opt = document.createElement('option');
                    opt.value = o.value; opt.textContent = o.label;
                    inp.appendChild(opt);
                });
                if (f.value != null) inp.value = String(f.value);
                return;
            }
            inp.type = f.type || 'text';
            inp.value = f.value == null ? '' : String(f.value);
            if (f.placeholder) inp.placeholder = f.placeholder;
            if (f.maxlength) inp.maxLength = parseInt(f.maxlength, 10);
            if (f.pattern) inp.pattern = f.pattern;
        });
        const collect = () => { const o = {}; els.forEach(i => o[i.dataset.key] = i.value); return o; };
        const done = v => { overlay.remove(); document.removeEventListener('keydown', onKey); resolve(v); };
        const onKey = e => { if (e.key === 'Escape') done(null); else if (e.key === 'Enter' && e.target.tagName === 'INPUT') done(collect()); };
        overlay.addEventListener('click', e => {
            if (e.target === overlay) return done(null);
            const act = e.target.closest('[data-nf]');
            if (act) done(act.getAttribute('data-nf') === 'ok' ? collect() : null);
        });
        document.addEventListener('keydown', onKey);
        document.body.appendChild(overlay);
        if (els[0]) { els[0].focus(); if (els[0].tagName === 'INPUT') els[0].select(); }
    });
}

// ---- Tunnel edit helpers (shared by the dashboard owner + admin Users page) ----
// nfPrompt fields for editing a tunnel: subdomain (the public address), display
// name, and one label field per route. The route's mode:port is shown read-only in
// the field label because the local port is the AGENT's own machine port (set via
// `--route`, and part of the reservation's idempotency key) - not server-editable.
function tunnelEditFields(t) {
    const fields = [
        { key: 'subdomain', label: 'Subdomain', hint: `(.${t.public_host} - lowercase, 3–30 chars)`,
          value: t.subdomain, maxlength: 30, pattern: '[a-z0-9-]{3,30}' },
        { key: 'name', label: 'Display name', hint: '(optional)', value: t.name || '', maxlength: 60 },
    ];
    (t.routes || []).forEach(r => fields.push({
        key: 'label_' + r.route_id, label: `Route ${r.mode}:${r.local_port} - label`,
        hint: '(optional)', value: r.label || '', maxlength: 40,
    }));
    return fields;
}
// Map nfPrompt results back into an API.editTunnel() request body.
function tunnelEditBody(t, res) {
    return {
        subdomain: (res.subdomain || '').trim().toLowerCase(),
        name: (res.name || '').trim(),
        route_labels: (t.routes || []).map(r => ({ route_id: r.route_id, label: (res['label_' + r.route_id] || '').trim() })),
    };
}

// ---- Toast ----
function toast(message, variant = 'primary') {
    let wrap = document.querySelector('.toast-wrap');
    if (!wrap) { wrap = document.createElement('div'); wrap.className = 'toast-wrap'; document.body.appendChild(wrap); }
    const el = document.createElement('div');
    el.className = `toast ${variant === 'danger' ? 'danger' : variant === 'success' ? 'success' : ''}`;
    el.textContent = message;
    wrap.appendChild(el);
    setTimeout(() => el.remove(), 4200);
}

document.addEventListener('DOMContentLoaded', () => {
    nfRenderIcons();
    nfTabs();
    nfWireModals();
    applyRoleVisibility();
});
