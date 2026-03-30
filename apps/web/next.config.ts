import type { NextConfig } from 'next';

const nextConfig: NextConfig = {
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
