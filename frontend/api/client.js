/**
 * Thesis Proxy - Professional Frontend API Client
 * Interfaces securely with the `website_backend` for Auth/Billing
 * and the `core_proxy_backend` for Anycast Tunnel allocations.
 */

class ProxyAPIClient {
    constructor() {
        this.authBaseUrl = 'http://127.0.0.1:3000/api'; // website_backend
        this.coreBaseUrl = 'http://127.0.0.1:3001/internal'; // core_proxy_backend
        this.jwtToken = localStorage.getItem('jwt_token') || null;
    }

    // --- Authentication Flow ---
    
    async webLogin(email, password) {
        const response = await fetch(`${this.authBaseUrl}/auth/login`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ email, password })
        });
        const data = await response.json();
        if (data.token) {
            this.jwtToken = data.token;
            localStorage.setItem('jwt_token', data.token);
        }
        return data;
    }

    // --- Tunnel Management ---

    async requestTunnel() {
        const response = await fetch(`${this.authBaseUrl}/tunnels/request`, {
            method: 'POST',
            headers: { 
                'Content-Type': 'application/json',
                'Authorization': `Bearer ${this.jwtToken}`
            }
        });
        return await response.json();
    }

    async getActiveTunnels() {
        const response = await fetch(`${this.authBaseUrl}/tunnels`, {
            headers: { 'Authorization': `Bearer ${this.jwtToken}` }
        });
        return await response.json();
    }

    // --- Admin Flow (Region/DDoS Blocks) ---

    async banRegion(countryCode) {
        const response = await fetch(`${this.authBaseUrl}/admin/region_blocks`, {
            method: 'POST',
            headers: { 
                'Content-Type': 'application/json',
                'Authorization': `Bearer ${this.jwtToken}`
            },
            body: JSON.stringify({ country_code: countryCode })
        });
        return await response.json();
    }
}

// Export for global usage across the `/views`
window.API = new ProxyAPIClient();
