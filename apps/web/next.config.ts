import type { NextConfig } from 'next';

const nextConfig: NextConfig = {
    // Allow Tailscale and local network access for dev HMR
    allowedDevOrigins: ['100.76.9.23', '10.0.0.*', '*.ts.net'],

    // Leader API proxy for local dev
    async rewrites() {
        return [
            {
                source: '/api/:path*',
                destination: `${process.env.LEADER_URL ?? 'http://localhost:4400'}/api/:path*`,
            },
        ];
    },
};

export default nextConfig;
