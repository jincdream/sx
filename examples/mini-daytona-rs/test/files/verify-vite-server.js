/**
 * Verification script: starts the static server, makes an HTTP request,
 * validates the response, then exits.
 * This script is written into the sandbox and executed via the exec API.
 */
const http = require('node:http');
const { fork } = require('node:child_process');
const path = require('node:path');

const serverScriptPath = path.resolve(__dirname, 'serve-vite-dist.js');

// Start the server as a child process
const server = fork(serverScriptPath, [], {
    stdio: ['ignore', 'pipe', 'pipe', 'ipc']
});

// Give the server time to start
setTimeout(() => {
    http.get('http://localhost:8080', (res) => {
        let body = '';
        res.on('data', c => body += c);
        res.on('end', () => {
            console.log('HTTP_STATUS:' + res.statusCode);
            console.log('RESPONSE_LENGTH:' + body.length);
            if (body.includes('</html>')) console.log('FOUND_HTML_CLOSE');
            if (body.includes('<script')) console.log('FOUND_SCRIPT_TAG');
            console.log('BODY_PREVIEW:' + body.substring(0, 500));
            server.kill();
            process.exit(0);
        });
    }).on('error', (e) => {
        console.error('HTTP_ERROR:' + e.message);
        server.kill();
        process.exit(1);
    });
}, 2000);
