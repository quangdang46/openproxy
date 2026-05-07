const backendBaseUrl = (process.env.NEXT_PUBLIC_BASE_URL ?? process.env.BASE_URL ?? "http://127.0.0.1:4623").replace(/\/+$/, "");

/** @type {import('next').NextConfig} */
const nextConfig = {
  allowedDevOrigins: ["127.0.0.1", "localhost"],
  output: "standalone",
  images: {
    unoptimized: true
  },
  env: {},
  webpack: (config, { isServer }) => {
    // Ignore fs/path modules in browser bundle
    if (!isServer) {
      config.resolve.fallback = {
        ...config.resolve.fallback,
        fs: false,
        path: false,
      };
    }
    // Stop watching logs directory to prevent HMR during streaming
    config.watchOptions = { ...config.watchOptions, ignored: /[\\/](logs|\.next)[\\/]/ };
    return config;
  },
  async rewrites() {
    return {
      afterFiles: [
        {
          source: "/v1/v1/:path*",
          destination: "/api/v1/:path*"
        },
        {
          source: "/v1/v1",
          destination: "/api/v1"
        },
        {
          source: "/codex/:path*",
          destination: "/api/v1/responses"
        },
        {
          source: "/v1beta/:path*",
          destination: "/api/v1beta/:path*"
        },
        {
          source: "/v1/:path*",
          destination: "/api/v1/:path*"
        },
        {
          source: "/v1",
          destination: "/api/v1"
        }
      ],
      fallback: [
        {
          source: "/api/v1/:path*",
          destination: `${backendBaseUrl}/v1/:path*`
        },
        {
          source: "/api/v1",
          destination: `${backendBaseUrl}/v1`
        },
        {
          source: "/api/v1beta/:path*",
          destination: `${backendBaseUrl}/v1beta/:path*`
        },
        {
          source: "/api/:path*",
          destination: `${backendBaseUrl}/api/:path*`
        }
      ]
    };
  }
};

export default nextConfig;
