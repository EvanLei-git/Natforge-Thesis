// Read-only device -> service-host tree for non-dashboard user pages (e.g. Profile), so
// the sidebar looks and behaves the same everywhere. It renders into #deviceTree; the
// dashboard has its own interactive tree, so this only runs where that isn't present.
(function () {
    const host = document.getElementById('deviceTree');
    if (!host || typeof API === 'undefined' || window.nfDashboardTree) return;
    if (API.role === 'admin') return; // admins use the Network/Users sidebar, not the device tree

    async function render() {
        let devices = [], tunnels = [];
        try { [devices, tunnels] = await Promise.all([API.getDevices(), API.getTunnels()]); }
        catch (_) { return; }
        const byDevice = {};
        tunnels.forEach(t => { if (t.device_id) (byDevice[t.device_id] = byDevice[t.device_id] || []).push(t); });
        let html = devices.map((d, i) => {
            const svcs = byDevice[d.id] || [];
            const rows = svcs.map((t, j) =>
                `<div class="tree-row tree-service" onclick="location.href='/dashboard?open=${t.tunnel_id}'">
                    <span class="tree-dot ${t.status}"></span><span class="tree-label">${escapeHtml(serviceLabel(t, j))}</span>
                 </div>`).join('');
            return `<div class="tree-device">
                <div class="tree-row tree-device-row" onclick="location.href='/dashboard?device=${d.id}'">
                    <span class="tree-chev open">${nfIcon('chevron')}</span>
                    <span class="tree-label"><strong>${escapeHtml(deviceLabel(d, i))}</strong></span>
                    <span class="tree-badge badge ${d.status}">${escapeHtml(d.status)}</span>
                </div>
                <div class="tree-children">${rows}</div>
            </div>`;
        }).join('');
        if (!devices.length) html = '<p class="tree-empty">No devices yet. Open the dashboard to add one.</p>';
        host.innerHTML = html;
    }

    render();
    setInterval(render, 8000);
})();
