#!/usr/bin/env node

const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');

const { getBuildPath, getSandboxPath } = require('./platform');

const API_BASE = 'http://localhost:3000/api';

async function request(method, route, body = null) {
    return new Promise((resolve, reject) => {
        const url = new URL(`${API_BASE}${route}`);
        const options = {
            hostname: url.hostname,
            port: url.port,
            path: url.pathname + url.search,
            method: method,
            headers: {},
            timeout: 3600000
        };

        if (body) {
            const bodyStr = JSON.stringify(body);
            options.headers['Content-Type'] = 'application/json';
            options.headers['Content-Length'] = Buffer.byteLength(bodyStr);
            body = bodyStr;
        }

        const req = http.request(options, (res) => {
            let data = '';
            res.on('data', chunk => data += chunk);
            res.on('end', () => {
                try {
                    resolve({ status: res.statusCode, data: JSON.parse(data) });
                } catch (e) {
                    resolve({ status: res.statusCode, data: null });
                }
            });
        });

        req.on('error', reject);
        req.on('timeout', () => {
            req.destroy();
            reject(new Error('timeout'));
        });

        if (body) {
            req.write(body);
        }
        req.end();
    });
}

const perfTracker = {
    records: [],
    add(test, operation, ms) {
        this.records.push({ test, operation, duration_ms: Math.round(ms) });
    }
};

async function timedRequest(test, operation, method, route, body) {
    const t0 = performance.now();
    const result = await request(method, route, body);
    const elapsed = performance.now() - t0;
    perfTracker.add(test, operation, elapsed);
    return result;
}

function printPerfReport() {
    const sep = '='.repeat(70);
    const dash = '-'.repeat(70);
    console.log(`\n${sep}`);
    console.log('📊 Performance Report');
    console.log(sep);
    console.log(`${'Test'.padEnd(22)}${'Operation'.padEnd(22)}${'Duration'.padStart(12)}`);
    console.log(dash);

    let total = 0;
    for (const r of perfTracker.records) {
        total += r.duration_ms;
        console.log(`${r.test.padEnd(22)}${r.operation.padEnd(22)}${(r.duration_ms + ' ms').padStart(12)}`);
    }

    console.log(dash);
    console.log(`${'Total test time:'.padEnd(44)}${(total + ' ms').padStart(12)}`);
    console.log(sep);
}

async function main() {
    console.log('--- Starting Vue3 Vite Environment Test ---');
    console.log(`Target: ${API_BASE}`);

    const buildScriptPath = getSandboxPath('build-vite-project.sh');
    const distDirPath = getSandboxPath('my-vue-app', 'dist');
    const serverScriptPath = getSandboxPath('serve-vite-dist.js');
    const verifyScriptPath = getSandboxPath('verify-vite-server.js');

    // 1. Build Snapshot (Vue3 Vite)
    console.log('\n[1] Requesting Vue3 Vite Build...');
    const buildRes = await timedRequest('Vite', 'build', 'POST', '/build', {
        dockerfile: getBuildPath('images/vue3-vite/Dockerfile'),
        context: getBuildPath('images/vue3-vite')
    });
    console.log('Vite Build Response:', buildRes.data);
    assert(buildRes.data.success === true, 'Vite Build failed');

    const snapshotPath = buildRes.data.data.snapshot_path;
    assert(snapshotPath, 'Snapshot path is missing');

    // 2. Start Sandbox
    console.log('\n[2] Requesting Vite Sandbox Start...');
    const startRes = await timedRequest('Vite', 'start', 'POST', '/start', {
        snapshot: snapshotPath
    });
    console.log('Vite Start Response:', startRes.data);
    assert(startRes.data.success === true, 'Vite Start failed');

    const sandboxId = startRes.data.data.sandbox_id;
    assert(sandboxId, 'Sandbox ID is missing');

    await new Promise(r => setTimeout(r, 3000));

    // 3. Write Build Script
    console.log('\n[3] Writing build script to Sandbox...');
    const buildScript = fs.readFileSync(path.resolve(__dirname, 'files', 'build-vite-project.sh')).toString('utf8');
    const writeRes = await timedRequest('Vite', 'file_write', 'POST', `/sandbox/${sandboxId}/file`, {
        path: buildScriptPath,
        content: buildScript
    });
    console.log('Write Response:', writeRes.data);
    assert(writeRes.data.success === true, 'Build script write failed');

    // 4. Execute Build Script
    console.log('\n[4] Executing build script (this may take a while)...');
    const buildExecRes = await timedRequest('Vite', 'exec_build', 'POST', `/sandbox/${sandboxId}/exec`, {
        cmd: ['bash', buildScriptPath]
    });
    console.log('Build Exec stdout:', buildExecRes.data?.data?.stdout?.slice(-500));
    console.log('Build Exec stderr:', buildExecRes.data?.data?.stderr?.slice(-500));
    assert(buildExecRes.data.success === true, 'Build exec failed');
    assert(buildExecRes.data.data.stdout.includes('SUCCESS: dist/index.html exists'), 'Build output missing success message');

    // 5. Verify build output
    console.log('\n[5] Verifying build output...');
    const verifyRes = await timedRequest('Vite', 'verify_build', 'POST', `/sandbox/${sandboxId}/exec`, {
        cmd: ['ls', '-la', distDirPath]
    });
    console.log('Verify Response:', verifyRes.data);
    assert(verifyRes.data.success === true, 'Verify exec failed');
    assert(verifyRes.data.data.stdout.includes('index.html'), 'dist/index.html not found in listing');

    // 6. Write Server Script
    console.log('\n[6] Writing static server script to Sandbox...');
    const serverScript = fs.readFileSync(path.resolve(__dirname, 'files', 'serve-vite-dist.js')).toString('utf8');
    const serverWriteRes = await timedRequest('Vite', 'file_write_server', 'POST', `/sandbox/${sandboxId}/file`, {
        path: serverScriptPath,
        content: serverScript
    });
    console.log('Server Script Write Response:', serverWriteRes.data);
    assert(serverWriteRes.data.success === true, 'Server script write failed');

    // 7. Write Verification Script
    console.log('\n[7] Writing verification script to Sandbox...');
    const verifyScript = fs.readFileSync(path.resolve(__dirname, 'files', 'verify-vite-server.js')).toString('utf8');
    const verifyWriteRes = await timedRequest('Vite', 'file_write_verify', 'POST', `/sandbox/${sandboxId}/file`, {
        path: verifyScriptPath,
        content: verifyScript
    });
    console.log('Verify Script Write Response:', verifyWriteRes.data);
    assert(verifyWriteRes.data.success === true, 'Verify script write failed');

    // 8. Start Server & Verify Access
    //    The verify script forks the server, makes an HTTP request, validates, then exits.
    console.log('\n[8] Starting Node server and verifying access...');
    const serverExecRes = await timedRequest('Vite', 'exec_server', 'POST', `/sandbox/${sandboxId}/exec`, {
        cmd: ['node', verifyScriptPath]
    });
    console.log('Server Exec stdout:', serverExecRes.data?.data?.stdout);
    console.log('Server Exec stderr:', serverExecRes.data?.data?.stderr);
    assert(serverExecRes.data.success === true, 'Server exec failed');
    assert(serverExecRes.data.data.stdout.includes('HTTP_STATUS:200'), 'Server did not return 200');
    assert(serverExecRes.data.data.stdout.includes('FOUND_HTML_CLOSE'), 'Response missing HTML close tag');

    // 9. Destroy Sandbox
    console.log('\n[9] Destroying Vite Sandbox...');
    const destroyRes = await timedRequest('Vite', 'destroy', 'DELETE', `/sandbox/${sandboxId}`);
    console.log('Destroy Response:', destroyRes.data);
    assert(destroyRes.data.success === true, 'Sandbox destroy failed');

    console.log('\n✅ Vue3 Vite test passed successfully!');
    printPerfReport();
}

main().catch(err => {
    console.error('\n❌ Test failed with error:', err);
    printPerfReport();
    process.exitCode = 1;
});
