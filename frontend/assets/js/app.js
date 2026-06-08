/**
 * NatForge — shared frontend helpers: auth guards, logout, toast notifications,
 * byte formatting.
 */

// Redirect to login if not authenticated. Optionally require the admin role.
function requireAuth(adminOnly = false) {
    if (!window.API.isAuthed()) {
        window.location.href = 'index.html';
        return false;
    }
    if (adminOnly && window.API.role !== 'admin') {
        toast('Administrator role required', 'danger');
        setTimeout(() => (window.location.href = 'dashboard.html'), 1200);
        return false;
    }
    return true;
}

function logout() {
    window.API.clearSession();
    window.location.href = 'index.html';
}

// Hide the admin nav link for non-admin users.
function applyRoleVisibility() {
    if (window.API && window.API.role !== 'admin') {
        document.querySelectorAll('.admin-only').forEach((el) => (el.style.display = 'none'));
    }
}

function fmtBytes(n) {
    if (n === undefined || n === null) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let v = Number(n);
    while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

// Minimal Bootstrap toast.
function toast(message, variant = 'primary') {
    let container = document.getElementById('nf-toast-container');
    if (!container) {
        container = document.createElement('div');
        container.id = 'nf-toast-container';
        container.className = 'toast-container position-fixed bottom-0 end-0 p-3';
        container.style.zIndex = 1080;
        document.body.appendChild(container);
    }
    const el = document.createElement('div');
    el.className = `toast align-items-center text-bg-${variant} border-0 show`;
    el.innerHTML = `<div class="d-flex"><div class="toast-body">${message}</div>
        <button type="button" class="btn-close btn-close-white me-2 m-auto" onclick="this.closest('.toast').remove()"></button></div>`;
    container.appendChild(el);
    setTimeout(() => el.remove(), 4000);
}

document.addEventListener('DOMContentLoaded', () => {
    applyRoleVisibility();
});
