/**
 * Zero-dependency Node.js static file server for serving Vite build output.
 * Serves files from the sandbox workspace dist directory.
 * Listens on 0.0.0.0:8080
 */
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');

const DIST_DIR = path.resolve(__dirname, 'my-vue-app', 'dist');
const PORT = 8080;
const HOST = '0.0.0.0';

const MIME_TYPES = {
    '.html': 'text/html; charset=utf-8',
    '.js': 'application/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.png': 'image/png',
    '.jpg': 'image/jpeg',
    '.jpeg': 'image/jpeg',
    '.gif': 'image/gif',
    '.svg': 'image/svg+xml',
    '.ico': 'image/x-icon',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
    '.ttf': 'font/ttf',
    '.txt': 'text/plain; charset=utf-8',
};

const server = http.createServer((req, res) => {
    let urlPath = req.url.split('?')[0];

    // Default to index.html for root or directory requests
    if (urlPath === '/' || urlPath.endsWith('/')) {
        urlPath += 'index.html';
    }

    const filePath = path.join(DIST_DIR, urlPath);

    // Security: prevent path traversal
    const resolved = path.resolve(filePath);
    if (!resolved.startsWith(path.resolve(DIST_DIR))) {
        res.writeHead(403);
        res.end('Forbidden');
        return;
    }

    fs.readFile(resolved, (err, data) => {
        if (err) {
            // If file not found, serve index.html for SPA routing
            if (err.code === 'ENOENT') {
                fs.readFile(path.join(DIST_DIR, 'index.html'), (err2, indexData) => {
                    if (err2) {
                        res.writeHead(404);
                        res.end('Not Found');
                    } else {
                        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
                        res.end(indexData);
                    }
                });
            } else {
                res.writeHead(500);
                res.end('Internal Server Error');
            }
            return;
        }

        const ext = path.extname(resolved).toLowerCase();
        const contentType = MIME_TYPES[ext] || 'application/octet-stream';

        res.writeHead(200, { 'Content-Type': contentType });
        res.end(data);
    });
});

server.listen(PORT, HOST, () => {
    console.log(`Server running at http://${HOST}:${PORT}`);
    console.log(`Serving files from: ${DIST_DIR}`);
});
