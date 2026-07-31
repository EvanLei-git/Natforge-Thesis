/**
 * NatForge - Frontend API Client
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
    // routes: [{ mode, local_port, label? }, ...]; subdomain + nodeId optional
    requestTunnel(routes, subdomain, nodeId) {
        return this._req('POST', '/tunnels/request', {
            routes,
            subdomain: subdomain || null,
            node_id: nodeId || null,
        });
    }
    stopTunnel(tunnelId)  { return this._req('POST', `/tunnels/${encodeURIComponent(tunnelId)}/stop`); }
    deleteTunnel(tunnelId){ return this._req('DELETE', `/tunnels/${encodeURIComponent(tunnelId)}`); }
    renameTunnel(tunnelId, name) { return this._req('PATCH', `/tunnels/${encodeURIComponent(tunnelId)}`, { name: name || null }); }
    // Full edit: { subdomain?, name?, route_labels?: [{route_id, label}] }.
    editTunnel(tunnelId, body)   { return this._req('PATCH', `/tunnels/${encodeURIComponent(tunnelId)}`, body); }
    getRegions()          { return this._req('GET', '/regions'); }
    getTunnelBandwidth(id){ return this._req('GET', `/tunnels/${encodeURIComponent(id)}/bandwidth`); }
    getTunnelLogs(id)     { return this._req('GET', `/tunnels/${encodeURIComponent(id)}/logs`); }
    getTunnelRegionBlocks(id)       { return this._req('GET', `/tunnels/${encodeURIComponent(id)}/region_blocks`); }
    setTunnelRegionBlocks(id, codes){ return this._req('PUT', `/tunnels/${encodeURIComponent(id)}/region_blocks`, { country_codes: codes }); }
    setCustomDomain(id, domain)     { return this._req('PUT', `/tunnels/${encodeURIComponent(id)}/custom_domain`, { domain }); }
    clearCustomDomain(id)           { return this._req('DELETE', `/tunnels/${encodeURIComponent(id)}/custom_domain`); }
    migrateTunnel(id, node_id)      { return this._req('POST', `/tunnels/${encodeURIComponent(id)}/migrate`, { node_id }); }

    // --- User self-service (profile + password) ---
    getProfile()              { return this._req('GET', '/user/profile'); }
    updateProfile(email, name){ return this._req('PUT', '/user/profile', { email, name: name || null }); }
    changePassword(current_password, new_password) { return this._req('PUT', '/user/password', { current_password, new_password }); }

    // --- Admin ---
    getStats()                { return this._req('GET', '/admin/stats'); }
    getAllTunnels()           { return this._req('GET', '/admin/tunnels'); }
    getUsers()                { return this._req('GET', '/admin/users'); }
    setUserBan(userId, banned){ return this._req('PATCH', `/admin/users/${encodeURIComponent(userId)}`, { banned }); }
    deleteUser(userId)        { return this._req('DELETE', `/admin/users/${encodeURIComponent(userId)}`); }
    getRegionBlocks()         { return this._req('GET', '/admin/region_blocks'); }
    addRegionBlock(cc)        { return this._req('POST', '/admin/region_blocks', { country_code: cc }); }
    removeRegionBlock(cc)     { return this._req('DELETE', `/admin/region_blocks/${cc}`); }
    getPortBlocks()           { return this._req('GET', '/admin/port_blocks'); }
    addPortBlock(port)        { return this._req('POST', '/admin/port_blocks', { port }); }
    removePortBlock(port)     { return this._req('DELETE', `/admin/port_blocks/${port}`); }
    // --- Admin: nodes / regions ---
    getNodes()                { return this._req('GET', '/admin/nodes'); }
    updateNode(id, body)      { return this._req('PATCH', `/admin/nodes/${encodeURIComponent(id)}`, body); }
    deleteNode(id)            { return this._req('DELETE', `/admin/nodes/${encodeURIComponent(id)}`); }
}

window.API = new NatForgeAPI();
