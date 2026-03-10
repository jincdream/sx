#!/usr/bin/env node

const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');

const { getBuildPath, getSandboxPath } = require('./platform');

const API_BASE = 'http://localhost:3000/api';

async function getServerCapabilities() {
  try {
    const res = await request('GET', '/info');
    if (res.data?.success && res.data?.data) {
      return res.data.data;
    }
  } catch (_err) {
    // Fall through to conservative default.
  }

  return {
    os: 'unknown',
    degraded_mode: false,
    supports_image_exec: true
  };
}

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
  console.log('--- Starting Puppeteer Environment Test ---');
  console.log(`Target: ${API_BASE}`);

  const sandboxScriptPath = getSandboxPath('test-puppeteer.js');

  const serverCapabilities = await getServerCapabilities();
  if (serverCapabilities.supports_image_exec === false) {
    console.log('[Capability Check] Skipping Puppeteer suite because the server cannot run Linux image binaries/packages natively.');
    return;
  }

  // 1. Build Snapshot (Puppeteer)
  console.log('\n[1] Requesting Puppeteer Build...');
  const pbBuildRes = await timedRequest('Puppeteer', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/puppeteer/Dockerfile'),
    context: getBuildPath('images/puppeteer')
  });
  console.log('Puppeteer Build Response:', pbBuildRes.data);
  assert(pbBuildRes.data.success === true, 'Puppeteer Build failed');

  const pbSnapshotPath = pbBuildRes.data.data.snapshot_path;
  assert(pbSnapshotPath, 'Puppeteer Snapshot path is missing');

  // 2. Start Sandbox (Puppeteer)
  console.log('\n[2] Requesting Puppeteer Sandbox Start...');
  const pbStartRes = await timedRequest('Puppeteer', 'start', 'POST', '/start', {
    snapshot: pbSnapshotPath
  });
  console.log('Puppeteer Start Response:', pbStartRes.data);
  assert(pbStartRes.data.success === true, 'Puppeteer Start failed');

  const pbSandboxId = pbStartRes.data.data.sandbox_id;
  assert(pbSandboxId, 'Puppeteer Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));

  // 3. Write Puppeteer Script
  console.log('\n[3] Writing Puppeteer Script to Sandbox...');
  const pbScriptContent = fs.readFileSync(path.resolve(__dirname, 'files', 'test-puppeteer.js')).toString('utf8');
  const pbWriteRes = await timedRequest('Puppeteer', 'file_write', 'POST', `/sandbox/${pbSandboxId}/file`, {
    path: sandboxScriptPath,
    content: pbScriptContent
  });
  console.log('Puppeteer Write Response:', pbWriteRes.data);
  assert(pbWriteRes.data.success === true, 'Puppeteer script write failed');

  // 4. Exec Puppeteer Script
  console.log('\n[4] Executing Puppeteer Script...');
  const pbExecRes = await timedRequest('Puppeteer', 'exec', 'POST', `/sandbox/${pbSandboxId}/exec`, {
    cmd: ['node', sandboxScriptPath]
  });
  console.log('Puppeteer Exec Response:', pbExecRes.data);
  assert(pbExecRes.data.success === true, 'Puppeteer Exec failed');
  assert(pbExecRes.data.data.stdout.includes('Browser closed successfully!'), 'Puppeteer output missing success message');
  assert(pbExecRes.data.data.stdout.includes('Page title:'), 'Puppeteer title missing message');

  // 5. Destroy Sandbox (Puppeteer)
  console.log('\n[5] Destroying Puppeteer Sandbox...');
  const pbDestroyRes = await timedRequest('Puppeteer', 'destroy', 'DELETE', `/sandbox/${pbSandboxId}`);
  console.log('Puppeteer Destroy Response:', pbDestroyRes.data);
  assert(pbDestroyRes.data.success === true, 'Puppeteer Sandbox destroy failed');

  console.log('\n✅ Puppeteer test passed successfully!');
  printPerfReport();
}

main().catch(err => {
  console.error('\n❌ Test failed with error:', err);
  printPerfReport();
  process.exitCode = 1;
});
