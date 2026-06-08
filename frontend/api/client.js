/**
 * NatForge — Frontend API Client
 *
 * A thin wrapper over the website_backend REST API. Served from the same origin
 * as the backend, so all paths are relative ("/api/..."). Handles JWT storage,
 * the Authorization header, and a couple of small response helpers.
 */

class NatForgeAPI {
    constructor() {
        this.base = '/api';
        this.token = localStorage.getItem('nf_token') || null;
        this.role = localStorage.getItem('nf_role') || null;
    }

    setSession(token, role) {
        this.token = token;
        this.role = role || 'user';
        localStorage.setItem('nf_token', token);
        localStorage.setItem('nf_role', this.role);
    }

    clearSession() {
        this.token = null;
        this.role = null;
        localStorage.removeItem('nf_token');
        localStorage.removeItem('nf_role');
    }

    isAuthed() {
        return !!this.token;
    }

    async _req(method, path, body) {
        const headers = { 'Content-Type': 'application/json' };
        if (this.token) headers['Authorization'] = `Bearer ${this.token}`;
        const opts = { method, headers };
        if (body !== undefined) opts.body = JSON.stringify(body);
        const res = await fetch(`${this.base}${path}`, opts);
        const text = await res.text();
        let data;
        try { data = text ? JSON.parse(text) : {}; } catch (_) { data = { raw: text }; }
        if (!res.ok) {
            const msg = (data && (data.error || data.message)) || text || res.statusText;
            throw new Error(`${res.status}: ${msg}`);
        }
        return data;
    }

    // --- Authentication ---
    register(email, password) { return this._req('POST', '/auth/register', { email, password }); }
    login(email, password)    { return this._req('POST', '/auth/login', { email, password }); }
    approveDevice(userCode)   { return this._req('POST', '/auth/device', { user_code: userCode }); }

    // --- Service-host tunnels ---
    getTunnels()          { return this._req('GET', '/tunnels'); }
    // routes: [{ mode: 'http'|'https'|'tcp', local_port: <number> }, ...]
    requestTunnel(routes) { return this._req('POST', '/tunnels/request', { routes }); }
    stopTunnel(tunnelId)  { return this._req('DELETE', `/tunnels/${encodeURIComponent(tunnelId)}`); }

    // --- IP host / edge node ---
    getIpHostStatus()         { return this._req('GET', '/ip_host/status'); }
    setRelayStatus(active)    { return this._req('POST', '/ip_host/status', { active }); }
    updatePrefs(mbps, geo)    { return this._req('PUT', '/user/preferences', { max_bandwidth_mbps: mbps, geo_pref_only: geo }); }

    // --- Admin ---
    getStats()                { return this._req('GET', '/admin/stats'); }
    getAllTunnels()           { return this._req('GET', '/admin/tunnels'); }
    getRegionBlocks()         { return this._req('GET', '/admin/region_blocks'); }
    addRegionBlock(cc)        { return this._req('POST', '/admin/region_blocks', { country_code: cc }); }
    removeRegionBlock(cc)     { return this._req('DELETE', `/admin/region_blocks/${cc}`); }
    getPortBlocks()           { return this._req('GET', '/admin/port_blocks'); }
    addPortBlock(port)        { return this._req('POST', '/admin/port_blocks', { port }); }
    removePortBlock(port)     { return this._req('DELETE', `/admin/port_blocks/${port}`); }
}

window.API = new NatForgeAPI();
