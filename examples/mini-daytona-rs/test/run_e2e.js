#!/usr/bin/env node

const { spawn, spawnSync } = require('node:child_process');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

const CLIENT_ONLY = process.argv.includes('--client');
const SERVER_BINARY = process.env.BINARY_PATH || './target/release/mini-daytona-rs';
const SKIP_RUST_CHECKS = process.env.SKIP_RUST_CHECKS === '1' || !!process.env.BINARY_PATH;
const API_HOST = process.env.API_HOST || '127.0.0.1';
const API_PORT = process.env.API_PORT || '3000';
const API_BASE = process.env.API_BASE || `http://${API_HOST}:${API_PORT}/api`;

const testArg = process.argv.find(a => a.startsWith('--test='));
const TEST_FILTER = testArg ? testArg.split('=')[1].toLowerCase() : null;

function shouldRunTest(name) {
  if (!TEST_FILTER) return true;
  return name.toLowerCase().includes(TEST_FILTER);
}

// Setup environment (only needed when running the server locally)
if (!CLIENT_ONLY) {
  let baseHome = process.env.DAYTONA_HOME || '/var/run/daytona_home';
  process.env.HOME = baseHome;
  process.env.TMPDIR = path.join(baseHome, 'tmp');
  try {
    fs.mkdirSync(process.env.TMPDIR, { recursive: true });
  } catch (err) {
    if (err.code === 'EACCES' && !process.env.DAYTONA_HOME) {
      console.warn(`\n[WARN] Permission denied creating ${process.env.TMPDIR}`);
      console.warn(`[WARN] Falling back to /tmp/daytona_home for local E2E test`);
      console.warn(`[WARN] (If testing a Docker server, remember to pass '--client')\n`);
      baseHome = '/tmp/daytona_home';
      process.env.HOME = baseHome;
      process.env.DAYTONA_HOME = baseHome;
      process.env.TMPDIR = path.join(baseHome, 'tmp');
      fs.mkdirSync(process.env.TMPDIR, { recursive: true });
    } else {
      throw err;
    }
  }
}

const http = require('node:http');

function getLocalMacOSStatus() {
  return require('os').platform() === 'darwin';
}

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
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
      timeout: 3600000 // 1 hour
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

// ── Performance tracking ──────────────────────────────────────────────
const perfTracker = {
  records: [],
  globalStart: null,
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

/**
 * Stream exec via SSE. Sends { cmd, stream: true } and consumes SSE events.
 * @param {string} sandboxId
 * @param {string[]} cmd
 * @param {object} options - { waitForText, timeoutMs, onStdout, onStderr }
 * @returns {Promise<{ stdout: string, stderr: string, exitCode: number|null }>}
 */
async function streamExec(sandboxId, cmd, options = {}) {
  const { waitForText, timeoutMs = 60000, onStdout, onStderr } = options;
  return new Promise((resolve, reject) => {
    const url = new URL(`${API_BASE}/sandbox/${sandboxId}/exec`);
    const bodyStr = JSON.stringify({ cmd, stream: true });
    const reqOptions = {
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': Buffer.byteLength(bodyStr),
        'Accept': 'text/event-stream',
      },
      timeout: timeoutMs,
    };

    let stdout = '';
    let stderr = '';
    let exitCode = null;
    let resolved = false;

    const timer = setTimeout(() => {
      if (!resolved) {
        resolved = true;
        req.destroy();
        resolve({ stdout, stderr, exitCode });
      }
    }, timeoutMs);

    const req = http.request(reqOptions, (res) => {
      let buffer = '';
      res.on('data', (chunk) => {
        buffer += chunk.toString();
        // Parse SSE events from the buffer
        const parts = buffer.split('\n\n');
        buffer = parts.pop(); // keep incomplete last part
        for (const part of parts) {
          let eventType = 'message';
          let data = '';
          for (const line of part.split('\n')) {
            if (line.startsWith('event:')) eventType = line.slice(6).trim();
            else if (line.startsWith('data:')) data += line.slice(5);
          }
          if (eventType === 'stdout') {
            stdout += data;
            if (onStdout) onStdout(data);
            if (waitForText && stdout.includes(waitForText) && !resolved) {
              resolved = true;
              clearTimeout(timer);
              req.destroy();
              resolve({ stdout, stderr, exitCode });
            }
          } else if (eventType === 'stderr') {
            stderr += data;
            if (onStderr) onStderr(data);
            if (waitForText && stderr.includes(waitForText) && !resolved) {
              resolved = true;
              clearTimeout(timer);
              req.destroy();
              resolve({ stdout, stderr, exitCode });
            }
          } else if (eventType === 'exit') {
            exitCode = parseInt(data, 10);
            if (!resolved) {
              resolved = true;
              clearTimeout(timer);
              resolve({ stdout, stderr, exitCode });
            }
          }
        }
      });
      res.on('end', () => {
        if (!resolved) {
          resolved = true;
          clearTimeout(timer);
          resolve({ stdout, stderr, exitCode });
        }
      });
    });

    req.on('error', (err) => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        reject(err);
      }
    });
    req.on('timeout', () => {
      if (!resolved) {
        resolved = true;
        clearTimeout(timer);
        req.destroy();
        resolve({ stdout, stderr, exitCode });
      }
    });

    req.write(bodyStr);
    req.end();
  });
}

async function getSandboxUrl(sandboxId) {
  let attempts = 0;
  while (attempts < 5) {
    const res = await request('GET', `/sandbox/${sandboxId}/info`);
    if (res?.data?.success && res.data?.data?.ip) {
      return `http://${res.data.data.ip}`;
    }
    attempts++;
    await new Promise(r => setTimeout(r, 1000));
  }
  return null;
}

async function getSandboxInfo(sandboxId) {
  const res = await request('GET', `/sandbox/${sandboxId}/info`);
  if (!res?.data?.success || !res.data?.data) {
    throw new Error(`Failed to get sandbox info for ${sandboxId}`);
  }
  return res.data.data;
}

function formatBytes(bytes) {
  if (bytes == null) return 'n/a';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index++;
  }
  const digits = value >= 100 || index === 0 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${units[index]}`;
}

function formatCpuLimit(resources = {}) {
  if (resources.cpu_quota == null || resources.cpu_period == null || resources.cpu_period === 0) {
    return 'n/a';
  }
  const cores = resources.cpu_quota / resources.cpu_period;
  return `${cores.toFixed(2)} cores (${resources.cpu_quota}/${resources.cpu_period} us)`;
}

function formatCpuUsageWindow(firstInfo, secondInfo, intervalMs) {
  const firstUsage = firstInfo?.stats?.cpu_usage_usec;
  const secondUsage = secondInfo?.stats?.cpu_usage_usec;
  if (firstUsage == null || secondUsage == null || secondUsage < firstUsage || intervalMs <= 0) {
    return null;
  }

  const percentOfOneCore = ((secondUsage - firstUsage) / (intervalMs * 1000)) * 100;
  const parts = [`${percentOfOneCore.toFixed(1)}% of 1 core`];

  const quota = secondInfo?.resources?.cpu_quota;
  const period = secondInfo?.resources?.cpu_period;
  if (quota != null && period != null && period > 0) {
    const quotaCores = quota / period;
    if (quotaCores > 0) {
      const percentOfQuota = percentOfOneCore / quotaCores;
      parts.push(`${percentOfQuota.toFixed(1)}% of sandbox CPU quota`);
    }
  }

  return parts.join(', ');
}

function printSandboxPerformance(label, infoA, infoB, intervalMs) {
  const snapshot = infoB || infoA;
  if (!snapshot) return;

  const resources = snapshot.resources || {};
  const stats = snapshot.stats || {};
  const cpuDetails = [];
  const cpuWindow = formatCpuUsageWindow(infoA, infoB, intervalMs);
  if (cpuWindow) cpuDetails.push(cpuWindow);
  if (stats.cpu_percent != null) cpuDetails.push(`${stats.cpu_percent.toFixed(1)}% instant`);
  if (stats.cpu_usage_usec != null) cpuDetails.push(`${(stats.cpu_usage_usec / 1000).toFixed(1)} ms cumulative`);

  console.log(`\n[Perf] ${label}`);
  console.log(`  sandbox=${snapshot.id} pid=${snapshot.pid ?? 'n/a'} ip=${snapshot.ip ?? 'n/a'}`);
  console.log(`  limits: memory=${formatBytes(resources.memory_bytes)}, cpu=${formatCpuLimit(resources)}, pids=${resources.pids_max ?? 'n/a'}, disk=${formatBytes(resources.disk_bytes)}`);
  console.log(`  usage: memory=${formatBytes(stats.memory_current_bytes)}, peak=${formatBytes(stats.memory_peak_bytes)}, rss=${formatBytes(stats.process_resident_bytes)}, pids=${stats.pids_current ?? 'n/a'}`);
  console.log(`  cpu: ${cpuDetails.length > 0 ? cpuDetails.join(', ') : 'n/a'}`);
  if (stats.oom_kill_count > 0) {
    console.log(`  ⚠️  OOM kills: ${stats.oom_kill_count}`);
  }
}

async function captureSandboxPerformance(testName, sandboxId, label, intervalMs = 400) {
  try {
    const first = await getSandboxInfo(sandboxId);
    await sleep(intervalMs);
    const second = await getSandboxInfo(sandboxId);
    printSandboxPerformance(`${testName} - ${label}`, first, second, intervalMs);
    return second;
  } catch (err) {
    console.warn(`[Perf] ${testName} - ${label}: failed to collect stats: ${err.message}`);
    return null;
  }
}

async function getServerCapabilities() {
  const fallback = {
    os: getLocalMacOSStatus() && !CLIENT_ONLY ? 'macos' : 'unknown',
    degraded_mode: getLocalMacOSStatus() && !CLIENT_ONLY,
    supports_image_exec: !(getLocalMacOSStatus() && !CLIENT_ONLY)
  };

  try {
    const res = await timedRequest('Server', 'info', 'GET', '/info');
    if (res.data?.success && res.data?.data) {
      return res.data.data;
    }
  } catch (_err) {
    // Older servers may not expose /info yet; fall back to local heuristics.
  }

  return fallback;
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

  // Aggregate by operation type
  const byOp = {};
  for (const r of perfTracker.records) {
    if (!byOp[r.operation]) byOp[r.operation] = { sum: 0, count: 0 };
    byOp[r.operation].sum += r.duration_ms;
    byOp[r.operation].count += 1;
  }
  console.log('\nSummary by operation type:');
  for (const [op, v] of Object.entries(byOp)) {
    const avg = Math.round(v.sum / v.count);
    console.log(`  ${op.padEnd(20)} avg ${(avg + ' ms').padStart(8)}  (${v.count} calls)`);
  }

  // Aggregate by test
  const byTest = {};
  for (const r of perfTracker.records) {
    if (!byTest[r.test]) byTest[r.test] = 0;
    byTest[r.test] += r.duration_ms;
  }
  console.log('\nSummary by test suite:');
  for (const [t, ms] of Object.entries(byTest)) {
    console.log(`  ${t.padEnd(20)} ${(ms + ' ms').padStart(10)}`);
  }

  console.log(sep);
}

async function runTests() {
  const serverCapabilities = await getServerCapabilities();
  const canRunImageExec = serverCapabilities.supports_image_exec !== false;
  const skipImageExecReason = serverCapabilities.os === 'macos' && serverCapabilities.degraded_mode
    ? '[macOS Degraded Mode] Skipping image binary execution tests (server cannot run Linux image binaries natively).'
    : '[Capability Check] Skipping image binary execution tests.';

  const path = require('path');
  const projectRoot = path.join(__dirname, '..');
  const getBuildPath = (subPath) => CLIENT_ONLY ? `/var/run/daytona_home/${subPath}` : path.join(projectRoot, subPath);

  if (shouldRunTest('api') || shouldRunTest('nginx')) {
    console.log('--- Starting API Integration Test ---');

  // 1. Build Snapshot
  console.log('\n[1] Requesting Build...');
  const buildRes = await timedRequest('Nginx', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/nginx/Dockerfile'),
    context: getBuildPath('images/nginx')
  });

  console.log('Build Response:', buildRes.data);
  assert(buildRes.data.success === true, 'Build failed');

  const snapshotPath = buildRes.data.data.snapshot_path;
  assert(snapshotPath, 'Snapshot path is missing');

  // 2. Start Sandbox
  console.log('\n[2] Requesting Start...');
  const startRes = await timedRequest('Nginx', 'start', 'POST', '/start', {
    snapshot: snapshotPath
  });

  console.log('Start Response:', startRes.data);
  assert(startRes.data.success === true, 'Start failed');

  const sandboxId = startRes.data.data.sandbox_id;
  assert(sandboxId, 'Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('Nginx', sandboxId, 'after start');

  // 3. Exec 'ls -la'
  console.log('\n[3] Executing command in Sandbox...');
  const execRes = await timedRequest('Nginx', 'exec', 'POST', `/sandbox/${sandboxId}/exec`, {
    cmd: ['/bin/ls', '-la', '/usr/share/nginx/html']
  });

  console.log('Exec Response:', execRes.data);
  assert(execRes.data.success === true, 'Exec failed');

  // 4. File Write
  console.log('\n[4] Writing file to Sandbox...');
  const writeRes = await timedRequest('Nginx', 'file_write', 'POST', `/sandbox/${sandboxId}/file`, {
    path: '/usr/share/nginx/html/custom.js',
    content: 'console.log("Hello Node Integration Test!");'
  });
  console.log('Write Response:', writeRes.data);
  assert(writeRes.data.success === true, 'File write failed');

  // 5. File Read
  console.log('\n[5] Reading written file...');
  const readRes = await timedRequest('Nginx', 'file_read', 'GET', `/sandbox/${sandboxId}/file?path=/usr/share/nginx/html/custom.js`);
  console.log('Read Response:', readRes.data);
  assert(readRes.data.success === true, 'File read failed');
  assert(readRes.data.data === 'console.log("Hello Node Integration Test!");', 'File content mismatch');

  // 5.5 Start Nginx daemon and Test exposed URL
  console.log('\n[5.5] Starting Nginx daemon and checking exposed URL...');
  if (serverCapabilities.os === 'macos') {
    console.log('[macOS] Skipping Nginx networking test because the linux nginx binary cannot run natively.');
  } else {
    const nginxExecRes = await timedRequest('Nginx', 'exec_daemon', 'POST', `/sandbox/${sandboxId}/exec`, {
      cmd: ['sh', '-c', 'nginx >/dev/null 2>&1']
    });
    console.log('Nginx Exec Response:', nginxExecRes.data);
    assert(nginxExecRes.data.success === true, 'Nginx exec failed');
    await captureSandboxPerformance('Nginx', sandboxId, 'after nginx daemon start');

    // Wait for nginx to actually start
    await new Promise(r => setTimeout(r, 1000));

    const sandboxUrl = await getSandboxUrl(sandboxId);
    assert(sandboxUrl, 'Failed to get sandbox URL');
    console.log(`Sandbox URL retrieved: ${sandboxUrl}`);

    console.log('\n[5.6] Fetching index.html from Sandbox URL via an external container (secondary sandbox)...');
    
    // Start Sandbox B (Client Sandbox)
    console.log('\n  [5.6.1] Starting secondary sandbox as an external client...');
    const clientStartRes = await timedRequest('Nginx', 'start_client', 'POST', '/start', {
      snapshot: snapshotPath
    });
    assert(clientStartRes.data.success === true, 'Secondary client sandbox start failed');
    const clientSandboxId = clientStartRes.data.data.sandbox_id;
    
    await new Promise(r => setTimeout(r, 1000));

    console.log(`\n  [5.6.2] Executing wget inside secondary sandbox to fetch ${sandboxUrl}...`);
    const fetchUrlStr = `http://${new URL(sandboxUrl).hostname}:80/`;
    
    const wgetRes = await timedRequest('Nginx', 'exec_client', 'POST', `/sandbox/${clientSandboxId}/exec`, {
      cmd: ['wget', '-qO-', fetchUrlStr]
    });
    
    console.log('Secondary Sandbox Wget Response:', wgetRes.data);
    assert(wgetRes.data.success === true, 'Secondary sandbox exec failed');
    
    const pageContent = wgetRes.data.data.stdout || '';
    console.log(`Fetched page content from external container: ${pageContent.trim()}`);
    assert(pageContent.includes('Hello Mini Daytona'), 'Nginx content mismatch when accessed from an external container');

    console.log('\n  [5.6.3] Destroying secondary client sandbox...');
    await timedRequest('Nginx', 'destroy_client', 'DELETE', `/sandbox/${clientSandboxId}`);

    console.log('\n[5.7] Fetching index.html directly from the Host environment...');
    try {
      const hostPageContent = await new Promise((resolve, reject) => {
        const req = http.get(fetchUrlStr, { timeout: 3000 }, (res) => {
          let data = '';
          res.on('data', chunk => data += chunk);
          res.on('end', () => resolve(data));
        });
        req.on('error', reject);
        req.on('timeout', () => {
          req.destroy();
          reject(new Error('timeout'));
        });
      });
      console.log(`Fetched page content from Host: ${hostPageContent.trim()}`);
      assert(hostPageContent.includes('Hello Mini Daytona'), 'Host: Nginx content mismatch');
    } catch (err) {
      if (serverCapabilities.os === 'linux') {
        // On Linux host or Linux Docker, host access should ideally work, or fail gracefully if network routing on host differs
        console.warn(`[Warning] Host direct fetch failed (often expected on Mac Docker hosts): ${err.message}`);
      } else {
        throw err;
      }
    }
  }

  // 6. Suspend Sandbox
  console.log('\n[6] Suspending Sandbox...');
  const suspendRes = await timedRequest('Nginx', 'suspend', 'POST', `/sandbox/${sandboxId}/suspend`);
  console.log('Suspend Response:', suspendRes.data);
  assert(suspendRes.data.success === true, 'Sandbox suspend failed');

  await new Promise(r => setTimeout(r, 1000));

  // 7. Resume Sandbox
  console.log('\n[7] Resuming Sandbox...');
  const resumeRes = await timedRequest('Nginx', 'resume', 'POST', `/sandbox/${sandboxId}/resume`);
  console.log('Resume Response:', resumeRes.data);
  assert(resumeRes.data.success === true, 'Sandbox resume failed');

  // 8. Delete File
  console.log('\n[8] Deleting file...');
  const delRes = await timedRequest('Nginx', 'file_delete', 'DELETE', `/sandbox/${sandboxId}/file`, {
    path: '/usr/share/nginx/html/custom.js'
  });
  console.log('Delete Response:', delRes.data);
  assert(delRes.data.success === true, 'File delete failed');

  // 9. Verify DELETED File
  console.log('\n[9] Verifying file deletion (expect to fail)...');
  const deletedReadRes = await timedRequest('Nginx', 'file_read_deleted', 'GET', `/sandbox/${sandboxId}/file?path=/usr/share/nginx/html/custom.js`);
  console.log('Read Deleted File Response:', deletedReadRes.data);
  assert(deletedReadRes.data.success === false, 'File delete verification failed');

  // 10. Destroy Sandbox
  console.log('\n[10] Destroying Sandbox...');
  await captureSandboxPerformance('Nginx', sandboxId, 'before destroy');
  const destroyRes = await timedRequest('Nginx', 'destroy', 'DELETE', `/sandbox/${sandboxId}`);
  console.log('Destroy Response:', destroyRes.data);
  assert(destroyRes.data.success === true, 'Sandbox destroy failed');
  }

  // ===============================
  // Test 2: Python Environment Test
  // ===============================
  if (shouldRunTest('python')) {
    console.log('\n--- Starting Python Environment Test ---');

  // 1. Build Snapshot (Python)
  console.log('\n[Wait for 2s] Requesting Python Build...');
  await new Promise(r => setTimeout(r, 2000));
  const pyBuildRes = await timedRequest('Python', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/python/Dockerfile'),
    context: getBuildPath('images/python')
  });
  console.log('Python Build Response:', pyBuildRes.data);
  assert(pyBuildRes.data.success === true, 'Python Build failed');

  const pySnapshotPath = pyBuildRes.data.data.snapshot_path;
  assert(pySnapshotPath, 'Python Snapshot path is missing');

  // 2. Start Sandbox (Python)
  console.log('\n[2] Requesting Python Sandbox Start...');
  const pyStartRes = await timedRequest('Python', 'start', 'POST', '/start', {
    snapshot: pySnapshotPath
  });
  console.log('Python Start Response:', pyStartRes.data);
  assert(pyStartRes.data.success === true, 'Python Start failed');

  const pySandboxId = pyStartRes.data.data.sandbox_id;
  assert(pySandboxId, 'Python Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('Python', pySandboxId, 'after start');

  // 3. Write Python Script
  console.log('\n[3] Writing Python Script to Sandbox...');
  const pyScriptContent = `
import sys
import json
import datetime
import math

now = datetime.datetime.now()
date_str = now.strftime("%Y-%m-%d")

# Simple computation: sum of squares
numbers = [1, 2, 3, 4, 5]
sum_of_squares = sum(x*x for x in numbers)

output = {
    'status': 'ok',
    'message': 'Hello from Python!',
    'date': date_str,
    'sum_of_squares': sum_of_squares
}
print(json.dumps(output))
`;
  const pyWriteRes = await timedRequest('Python', 'file_write', 'POST', `/sandbox/${pySandboxId}/file`, {
    path: '/opt/script.py',
    content: pyScriptContent
  });
  console.log('Python Write Response:', pyWriteRes.data);
  assert(pyWriteRes.data.success === true, 'Python script write failed');

  // 4. Exec Python Script
  console.log('\n[4] Executing Python Script...');
  if (!canRunImageExec) {
    console.log(skipImageExecReason);
  } else {
    const pyExecRes = await timedRequest('Python', 'exec', 'POST', `/sandbox/${pySandboxId}/exec`, {
      cmd: ['python3', '/opt/script.py']
    });
    console.log('Python Exec Response:', pyExecRes.data);
    assert(pyExecRes.data.success === true, 'Python Exec failed');
    assert(pyExecRes.data.data.stdout.includes('"message": "Hello from Python!"'), 'Python output message mismatch');
    assert(pyExecRes.data.data.stdout.includes('"sum_of_squares": 55'), 'Python computation mismatch');
    assert(pyExecRes.data.data.stdout.includes('"date": "'), 'Python date output missing');
    await captureSandboxPerformance('Python', pySandboxId, 'after script exec');
  }

  // 5. Destroy Sandbox (Python)
  console.log('\n[5] Destroying Python Sandbox...');
  const pyDestroyRes = await timedRequest('Python', 'destroy', 'DELETE', `/sandbox/${pySandboxId}`);
  console.log('Python Destroy Response:', pyDestroyRes.data);
  assert(pyDestroyRes.data.success === true, 'Python Sandbox destroy failed');
  }

  // ===============================
  // Test 3: Data Analysis Environment Test
  // ===============================
  if (shouldRunTest('data') || shouldRunTest('analysis')) {
    console.log('\n--- Starting Data Analysis Environment Test ---');

  // 1. Build Snapshot (Data Analysis)
  console.log('\n[Wait for 2s] Requesting Data Analysis Build...');
  await new Promise(r => setTimeout(r, 2000));
  const daBuildRes = await timedRequest('DataAnalysis', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/data-analysis/Dockerfile'),
    context: getBuildPath('images/data-analysis')
  });
  console.log('Data Analysis Build Response:', daBuildRes.data);
  assert(daBuildRes.data.success === true, 'Data Analysis Build failed');

  const daSnapshotPath = daBuildRes.data.data.snapshot_path;
  assert(daSnapshotPath, 'Data Analysis Snapshot path is missing');

  // 2. Start Sandbox (Data Analysis)
  console.log('\n[2] Requesting Data Analysis Sandbox Start...');
  const daStartRes = await timedRequest('DataAnalysis', 'start', 'POST', '/start', {
    snapshot: daSnapshotPath
  });
  console.log('Data Analysis Start Response:', daStartRes.data);
  assert(daStartRes.data.success === true, 'Data Analysis Start failed');

  const daSandboxId = daStartRes.data.data.sandbox_id;
  assert(daSandboxId, 'Data Analysis Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('DataAnalysis', daSandboxId, 'after start');

  // 3. Write Python Script
  console.log('\n[3] Writing Data Analysis Script to Sandbox...');
  const daScriptContent = `
import pandas as pd
import numpy as np
import json

df = pd.DataFrame({'A': [1, 2, 3], 'B': [4, 5, 6]})
result = {
    'status': 'ok',
    'message': 'Data Analysis Test',
    'data': df.to_dict(orient='records')
}
print(json.dumps(result))
`;
  const daWriteRes = await timedRequest('DataAnalysis', 'file_write', 'POST', `/sandbox/${daSandboxId}/file`, {
    path: '/home/daytona/workspace/da_script.py',
    content: daScriptContent
  });
  console.log('Data Analysis Write Response:', daWriteRes.data);
  assert(daWriteRes.data.success === true, 'Data Analysis script write failed');

  // 4. Exec Python Script
  console.log('\n[4] Executing Data Analysis Script...');
  if (!canRunImageExec) {
    console.log(skipImageExecReason);
  } else {
    const daExecRes = await timedRequest('DataAnalysis', 'exec', 'POST', `/sandbox/${daSandboxId}/exec`, {
      cmd: ['python3', '/home/daytona/workspace/da_script.py']
    });
    console.log('Data Analysis Exec Response:', daExecRes.data);
    assert(daExecRes.data.success === true, 'Data Analysis Exec failed');
    assert(daExecRes.data.data.stdout.includes('"message": "Data Analysis Test"'), 'Data Analysis output message mismatch');
    assert(daExecRes.data.data.stdout.includes('"A": 1'), 'Data Analysis output dataframe content mismatch');
    await captureSandboxPerformance('DataAnalysis', daSandboxId, 'after script exec');
  }

  // 5. Destroy Sandbox (Data Analysis)
  console.log('\n[5] Destroying Data Analysis Sandbox...');
  const daDestroyRes = await timedRequest('DataAnalysis', 'destroy', 'DELETE', `/sandbox/${daSandboxId}`);
  console.log('Data Analysis Destroy Response:', daDestroyRes.data);
  assert(daDestroyRes.data.success === true, 'Data Analysis Sandbox destroy failed');
  }

  // ===============================
  // Test 4: File Upload (xlsx) + Python Processing Test
  // ===============================
  let ulSnapshotPath;
  if (shouldRunTest('upload') || shouldRunTest('excel') || shouldRunTest('resource') || shouldRunTest('limit')) {
    console.log('\n[Wait for 2s] Requesting Data Analysis Build for Upload/Resource Test...');
    await new Promise(r => setTimeout(r, 2000));
    const ulBuildRes = await timedRequest('Upload', 'build', 'POST', '/build', {
      dockerfile: getBuildPath('images/data-analysis/Dockerfile'),
      context: getBuildPath('images/data-analysis')
    });
    assert(ulBuildRes.data.success === true, 'Upload Test Build failed');
    ulSnapshotPath = ulBuildRes.data.data.snapshot_path;
  }

  if (shouldRunTest('upload') || shouldRunTest('excel')) {
    console.log('\n--- Starting File Upload & Excel Processing Test ---');

  // 2. Start Sandbox with custom resource limits
  console.log('\n[2] Starting sandbox with custom resource limits...');
  const ulStartRes = await timedRequest('Upload', 'start', 'POST', '/start', {
    snapshot: ulSnapshotPath,
    resources: {
      memory_bytes: 536870912,   // 512 MiB
      cpu_quota: 200000,         // 2 cores
      cpu_period: 100000,
      pids_max: 128
    }
  });
  console.log('Upload Test Start Response:', ulStartRes.data);
  assert(ulStartRes.data.success === true, 'Upload Test Start failed');

  const ulSandboxId = ulStartRes.data.data.sandbox_id;
  assert(ulSandboxId, 'Upload Test Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('Upload', ulSandboxId, 'after start');

  // 3. Read dataset.xlsx from disk and upload via binary upload API
  console.log('\n[3] Uploading dataset.xlsx to sandbox...');
  const xlsxPath = path.resolve(__dirname, 'dataset.xlsx');
  const xlsxBuffer = fs.readFileSync(xlsxPath);
  const xlsxBase64 = xlsxBuffer.toString('base64');
  console.log(`  File size: ${xlsxBuffer.length} bytes, base64 length: ${xlsxBase64.length}`);

  const uploadRes = await timedRequest('Upload', 'upload', 'POST', `/sandbox/${ulSandboxId}/upload`, {
    path: '/home/daytona/workspace/dataset.xlsx',
    data: xlsxBase64
  });
  console.log('Upload Response:', uploadRes.data);
  assert(uploadRes.data.success === true, 'File upload failed');

  // 4. Write a Python script that processes the xlsx
  console.log('\n[4] Writing xlsx processing script...');
  const xlsxScript = `
import pandas as pd
import json
import sys

try:
    df = pd.read_excel('/home/daytona/workspace/dataset.xlsx', engine='openpyxl')
    # Replace NaN with None so json.dumps outputs null instead of NaN
    df_clean = df.head(3).where(df.head(3).notna(), None)
    result = {
        'status': 'ok',
        'rows': len(df),
        'columns': list(df.columns),
        'dtypes': {col: str(dtype) for col, dtype in df.dtypes.items()},
        'head': df_clean.to_dict(orient='records'),
        'describe': {}
    }
    # Add numeric column stats (dropna to avoid NaN in stats)
    for col in df.select_dtypes(include=['number']).columns:
        col_data = df[col].dropna()
        if len(col_data) > 0:
            stats = col_data.describe()
            result['describe'][col] = {
                'mean': float(stats['mean']),
                'min': float(stats['min']),
                'max': float(stats['max']),
                'count': int(stats['count'])
            }
    print(json.dumps(result, default=str))
except Exception as e:
    print(json.dumps({'status': 'error', 'message': str(e)}))
    sys.exit(1)
`;
  const scriptWriteRes = await timedRequest('Upload', 'file_write', 'POST', `/sandbox/${ulSandboxId}/file`, {
    path: '/home/daytona/workspace/process_xlsx.py',
    content: xlsxScript
  });
  console.log('Script Write Response:', scriptWriteRes.data);
  assert(scriptWriteRes.data.success === true, 'Script write failed');

  // 5. Execute the script
  console.log('\n[5] Executing xlsx processing script...');
  if (!canRunImageExec) {
    console.log(skipImageExecReason);
  } else {
    const xlsxExecRes = await timedRequest('Upload', 'exec', 'POST', `/sandbox/${ulSandboxId}/exec`, {
      cmd: ['python3', '/home/daytona/workspace/process_xlsx.py']
    });
    console.log('Xlsx Exec Response:', xlsxExecRes.data);
    assert(xlsxExecRes.data.success === true, 'Xlsx exec failed');

    const xlsxOutput = JSON.parse(xlsxExecRes.data.data.stdout.trim());
    console.log('Parsed xlsx output:', JSON.stringify(xlsxOutput, null, 2));
    assert(xlsxOutput.status === 'ok', 'Xlsx processing status is not ok');
    assert(xlsxOutput.rows > 0, 'Xlsx has no rows');
    assert(xlsxOutput.columns.length > 0, 'Xlsx has no columns');
    console.log(`  Processed ${xlsxOutput.rows} rows, ${xlsxOutput.columns.length} columns`);
    await captureSandboxPerformance('Upload', ulSandboxId, 'after xlsx processing');
  }

  // 6. Download the file back and verify it matches
  console.log('\n[6] Downloading xlsx from sandbox to verify...');
  const downloadRes = await timedRequest('Upload', 'download', 'GET', `/sandbox/${ulSandboxId}/download?path=/home/daytona/workspace/dataset.xlsx`);
  assert(downloadRes.data.success === true, 'File download failed');
  const downloadedBuffer = Buffer.from(downloadRes.data.data, 'base64');
  assert(downloadedBuffer.length === xlsxBuffer.length, `Downloaded file size mismatch: ${downloadedBuffer.length} vs ${xlsxBuffer.length}`);
  console.log(`  Downloaded file size matches: ${downloadedBuffer.length} bytes`);

  // 7. Destroy Sandbox
  console.log('\n[7] Destroying Upload Test Sandbox...');
  const ulDestroyRes = await timedRequest('Upload', 'destroy', 'DELETE', `/sandbox/${ulSandboxId}`);
  console.log('Upload Test Destroy Response:', ulDestroyRes.data);
  assert(ulDestroyRes.data.success === true, 'Upload Test Sandbox destroy failed');
  }

  // ===============================
  // Test 5: Resource Limits Test
  // ===============================
  if (shouldRunTest('resource') || shouldRunTest('limit')) {
    console.log('\n--- Starting Resource Limits Test ---');

  // 1. Start sandbox with restricted resources
  console.log('\n[1] Starting sandbox with strict resource limits...');
  const rlStartRes = await timedRequest('ResourceLimits', 'start', 'POST', '/start', {
    snapshot: ulSnapshotPath,
    resources: {
      memory_bytes: 134217728,  // 128 MiB
      cpu_quota: 50000,         // 0.5 core
      cpu_period: 100000,
      pids_max: 32
    }
  });
  console.log('Resource Limits Start Response:', rlStartRes.data);
  assert(rlStartRes.data.success === true, 'Resource Limits Start failed');

  const rlSandboxId = rlStartRes.data.data.sandbox_id;
  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('ResourceLimits', rlSandboxId, 'after start');

  // 2. Verify sandbox is functional with limited resources
  console.log('\n[2] Verifying sandbox with limited resources...');
  if (!canRunImageExec) {
    console.log(skipImageExecReason);
  } else {
    const rlExecRes = await timedRequest('ResourceLimits', 'exec', 'POST', `/sandbox/${rlSandboxId}/exec`, {
      cmd: ['python3', '-c', 'import json; print(json.dumps({"status": "ok", "message": "Running with limited resources"}))']
    });
    console.log('Resource Limits Exec Response:', rlExecRes.data);
    assert(rlExecRes.data.success === true, 'Resource Limits exec failed');
    assert(rlExecRes.data.data.stdout.includes('"status": "ok"'), 'Resource Limits output mismatch');
    await captureSandboxPerformance('ResourceLimits', rlSandboxId, 'after limited-resource exec');
  }

  console.log('\n[3] Destroying Resource Limits Sandbox...');
  const rlDestroyRes = await timedRequest('ResourceLimits', 'destroy', 'DELETE', `/sandbox/${rlSandboxId}`);
  assert(rlDestroyRes.data.success === true, 'Resource Limits Sandbox destroy failed');
  }

  // ===============================
  // Test 6: Puppeteer Environment Test
  // ===============================
  if (shouldRunTest('puppeteer')) {
    console.log('\n--- Starting Puppeteer Environment Test ---');

  if (!canRunImageExec) {
    console.log('[Capability Check] Skipping Puppeteer suite (server cannot run image binaries).');
  } else {

  // 1. Build Snapshot (Puppeteer)
  console.log('\n[Wait for 2s] Requesting Puppeteer Build...');
  await new Promise(r => setTimeout(r, 2000));
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
  await captureSandboxPerformance('Puppeteer', pbSandboxId, 'after start');

  // 3. Write Puppeteer Script
  console.log('\n[3] Writing Puppeteer Script to Sandbox...');
  const pbScriptContent = fs.readFileSync(path.resolve(__dirname, 'files', 'test-puppeteer.js')).toString('utf8');
  const pbWriteRes = await timedRequest('Puppeteer', 'file_write', 'POST', `/sandbox/${pbSandboxId}/file`, {
    path: '/home/daytona/workspace/test-puppeteer.js',
    content: pbScriptContent
  });
  console.log('Puppeteer Write Response:', pbWriteRes.data);
  assert(pbWriteRes.data.success === true, 'Puppeteer script write failed');

  // 4. Exec Puppeteer Script
  console.log('\n[4] Executing Puppeteer Script...');
  if (!canRunImageExec) {
    console.log(skipImageExecReason);
  } else {
    const pbExecRes = await timedRequest('Puppeteer', 'exec', 'POST', `/sandbox/${pbSandboxId}/exec`, {
      cmd: ['node', '/home/daytona/workspace/test-puppeteer.js']
    });
    console.log('Puppeteer Exec Response:', pbExecRes.data);
    assert(pbExecRes.data.success === true, 'Puppeteer Exec failed');
    assert(pbExecRes.data.data.stdout.includes('Browser closed successfully!'), 'Puppeteer output missing success message');
    assert(pbExecRes.data.data.stdout.includes('Page title:'), 'Puppeteer title missing message');
    await captureSandboxPerformance('Puppeteer', pbSandboxId, 'after puppeteer exec');
  }

  // 5. Destroy Sandbox (Puppeteer)
  console.log('\n[5] Destroying Puppeteer Sandbox...');
  const pbDestroyRes = await timedRequest('Puppeteer', 'destroy', 'DELETE', `/sandbox/${pbSandboxId}`);
  console.log('Puppeteer Destroy Response:', pbDestroyRes.data);
  assert(pbDestroyRes.data.success === true, 'Puppeteer Sandbox destroy failed');
  }
  }

  // ===============================
  // Test 7: Next.js Environment Test
  // ===============================
  if (shouldRunTest('nextjs') || shouldRunTest('next')) {
    console.log('\n--- Starting Next.js Environment Test ---');

  if (!canRunImageExec) {
    console.log('[Capability Check] Skipping Next.js test (server cannot run image binaries).');
  } else {
  // 1. Build Snapshot (Next.js)
  console.log('\n[Wait for 2s] Requesting Next.js Build...');
  await new Promise(r => setTimeout(r, 2000));
  const nextBuildRes = await timedRequest('Next.js', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/nextjs/Dockerfile'),
    context: getBuildPath('images/nextjs')
  });
  console.log('Next.js Build Response:', nextBuildRes.data);
  assert(nextBuildRes.data.success === true, 'Next.js Build failed');

  const nextSnapshotPath = nextBuildRes.data.data.snapshot_path;
  assert(nextSnapshotPath, 'Next.js Snapshot path is missing');

  // 2. Start Sandbox (Next.js)
  console.log('\n[2] Requesting Next.js Sandbox Start...');
  const nextStartRes = await timedRequest('Next.js', 'start', 'POST', '/start', {
    snapshot: nextSnapshotPath
  });
  console.log('Next.js Start Response:', nextStartRes.data);
  const nextSandboxId = nextStartRes.data.data.sandbox_id;
  assert(nextSandboxId, 'Next.js Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));
  await captureSandboxPerformance('Next.js', nextSandboxId, 'after start');

  // 3. Create Next.js App
  console.log('\\n[3] Creating Next.js App (this may take a minute)...');
  const nextCreateRes = await timedRequest('Next.js', 'exec', 'POST', `/sandbox/${nextSandboxId}/exec`, {
    cmd: ['create-next-app', '/home/daytona/workspace/my-app', '--yes', '--javascript', '--no-tailwind', '--no-eslint', '--app', '--no-src-dir', '--import-alias', '@/*', '--use-npm']
  });
  console.log('Next.js Create Response:', nextCreateRes.data);
  assert(nextCreateRes.data.success === true, 'Next.js app creation failed');

  // Override config to bind to 0.0.0.0
  console.log('\\n[4] Modifying package.json to bind to 0.0.0.0...');
  const sedRes = await timedRequest('Next.js', 'exec', 'POST', `/sandbox/${nextSandboxId}/exec`, {
    cmd: ['sed', '-i', 's/"dev": "next dev"/"dev": "next dev -H 0.0.0.0"/', '/home/daytona/workspace/my-app/package.json']
  });
  console.log('sed Response:', sedRes.data);

  // Add custom test content
  const pageContent = `export default function Home() { return <div>Hello Next.js Daytona!</div>; }`;
  await timedRequest('Next.js', 'file_write', 'POST', `/sandbox/${nextSandboxId}/file`, {
    path: '/home/daytona/workspace/my-app/app/page.js',
    content: pageContent
  });

  // 5. Start Next.js dev server via SSE streaming
  console.log('\n[5] Starting Next.js dev server (SSE streaming)...');
  const t0dev = performance.now();
  const devResult = await streamExec(nextSandboxId,
    ['sh', '-c', 'cd /home/daytona/workspace/my-app && npm run dev'],
    {
      waitForText: 'Ready',
      timeoutMs: 120000,
      onStdout: (chunk) => process.stdout.write(`[next:stdout] ${chunk}\n`),
      onStderr: (chunk) => process.stderr.write(`[next:stderr] ${chunk}\n`),
    }
  );
  perfTracker.add('Next.js', 'dev_server_start', performance.now() - t0dev);
  console.log(`\nNext.js dev server streaming finished. exitCode=${devResult.exitCode}`);
  console.log('stdout length:', devResult.stdout.length, 'stderr length:', devResult.stderr.length);

  // Give the server a moment after Ready signal
  await new Promise(r => setTimeout(r, 3000));

  // 6. Fetch from Next.js server via host-level proxy endpoint
  //    The API server proxies requests to the sandbox's internal IP:port
  //    Host accesses: http://localhost:3000/api/sandbox/{id}/proxy/3000/
  const proxyUrl = `/sandbox/${nextSandboxId}/proxy/3000`;
  console.log(`\n[6] Fetching Next.js via host proxy: ${API_BASE}${proxyUrl} ...`);

  const curlRes = await timedRequest('Next.js', 'proxy_fetch', 'GET', proxyUrl);
  console.log('Host proxy fetch status:', curlRes.status);

  assert(curlRes.status === 200, `Next.js proxy fetch returned status ${curlRes.status}`);
  // The proxy returns raw HTML, not JSON — curlRes.data may be null if not JSON
  // Use a raw HTTP request to get the body as text
  const proxyContent = await new Promise((resolve, reject) => {
    const url = new URL(`${API_BASE}${proxyUrl}`);
    const fetchReq = http.get({
      hostname: url.hostname,
      port: url.port,
      path: url.pathname,
      timeout: 15000,
    }, (res) => {
      let body = '';
      res.on('data', chunk => body += chunk);
      res.on('end', () => resolve(body));
    });
    fetchReq.on('error', reject);
    fetchReq.on('timeout', () => { fetchReq.destroy(); reject(new Error('Proxy fetch timeout')); });
  });
  console.log('Host proxy response length:', proxyContent.length);
  console.log('Host proxy preview:', proxyContent.substring(0, 200));
  assert(proxyContent.includes('Hello Next.js Daytona!'), 'Next.js content mismatch via host proxy');
  await captureSandboxPerformance('Next.js', nextSandboxId, 'after dev server ready');

  console.log('\n[7] Destroying Next.js Sandbox...');
  const nextDestroyRes = await timedRequest('Next.js', 'destroy', 'DELETE', `/sandbox/${nextSandboxId}`);
  console.log('Next.js Destroy Response:', nextDestroyRes.data);
  assert(nextDestroyRes.data.success === true, 'Next.js Sandbox destroy failed');
  }
  }

  // ===============================
  // Test 8: Shared Volume Mount Test
  // ===============================
  if (shouldRunTest('volume') || shouldRunTest('mount')) {
    console.log('\n--- Starting Shared Volume Mount Test ---');

  // 1. Create a volume
  console.log('\n[1] Creating a shared volume...');
  const volCreateRes = await timedRequest('Volume', 'create', 'POST', '/volumes', {
    name: 'e2e-shared-vol'
  });
  console.log('Volume Create Response:', volCreateRes.data);
  assert(volCreateRes.data.success === true, 'Volume creation failed');
  const volumeId = volCreateRes.data.data.id;
  const volumeName = volCreateRes.data.data.name;
  assert(volumeId, 'Volume ID is missing');
  assert(volumeName === 'e2e-shared-vol', 'Volume name mismatch');

  // 2. List volumes and verify
  console.log('\n[2] Listing volumes...');
  const volListRes = await timedRequest('Volume', 'list', 'GET', '/volumes');
  console.log('Volume List Response:', volListRes.data);
  assert(volListRes.data.success === true, 'Volume list failed');
  const foundVol = volListRes.data.data.find(v => v.id === volumeId);
  assert(foundVol, 'Created volume not found in list');
  assert(foundVol.name === 'e2e-shared-vol', 'Volume name mismatch in list');

  // 3. Build snapshot for volume test
  console.log('\n[3] Building snapshot for volume test...');
  await new Promise(r => setTimeout(r, 2000));
  const volBuildRes = await timedRequest('Volume', 'build', 'POST', '/build', {
    dockerfile: getBuildPath('images/nginx/Dockerfile'),
    context: getBuildPath('images/nginx')
  });
  assert(volBuildRes.data.success === true, 'Volume test build failed');
  const volSnapshotPath = volBuildRes.data.data.snapshot_path;

  // 4. Start Sandbox A with volume mounted (read-write)
  console.log('\n[4] Starting Sandbox A with shared volume...');
  const sandboxARes = await timedRequest('Volume', 'start_a', 'POST', '/start', {
    snapshot: volSnapshotPath,
    mounts: [{ volume_id: volumeId, mount_path: '/shared-data', readonly: false }]
  });
  console.log('Sandbox A Start Response:', sandboxARes.data);
  assert(sandboxARes.data.success === true, 'Sandbox A start failed');
  const sandboxAId = sandboxARes.data.data.sandbox_id;

  await new Promise(r => setTimeout(r, 3000));

  // 5. Write a file into the shared volume via Sandbox A
  console.log('\n[5] Writing file to shared volume via Sandbox A...');
  const volWriteRes = await timedRequest('Volume', 'file_write_a', 'POST', `/sandbox/${sandboxAId}/file`, {
    path: '/shared-data/hello.txt',
    content: 'Hello from Sandbox A!'
  });
  console.log('Volume Write Response:', volWriteRes.data);
  assert(volWriteRes.data.success === true, 'Volume file write via Sandbox A failed');

  // 6. Read back from Sandbox A to confirm
  console.log('\n[6] Reading file from shared volume via Sandbox A...');
  const volReadARes = await timedRequest('Volume', 'file_read_a', 'GET', `/sandbox/${sandboxAId}/file?path=/shared-data/hello.txt`);
  console.log('Volume Read (A) Response:', volReadARes.data);
  assert(volReadARes.data.success === true, 'Volume file read via Sandbox A failed');
  assert(volReadARes.data.data === 'Hello from Sandbox A!', 'Volume file content mismatch in Sandbox A');

  // 7. Start Sandbox B with the same volume mounted (read-only)
  console.log('\n[7] Starting Sandbox B with same shared volume (read-only)...');
  const sandboxBRes = await timedRequest('Volume', 'start_b', 'POST', '/start', {
    snapshot: volSnapshotPath,
    mounts: [{ volume_id: volumeId, mount_path: '/shared-data', readonly: true }]
  });
  console.log('Sandbox B Start Response:', sandboxBRes.data);
  assert(sandboxBRes.data.success === true, 'Sandbox B start failed');
  const sandboxBId = sandboxBRes.data.data.sandbox_id;

  await new Promise(r => setTimeout(r, 3000));

  // 8. Read the file from Sandbox B — verifying cross-sandbox volume sharing
  console.log('\n[8] Reading file from shared volume via Sandbox B...');
  const volReadBRes = await timedRequest('Volume', 'file_read_b', 'GET', `/sandbox/${sandboxBId}/file?path=/shared-data/hello.txt`);
  console.log('Volume Read (B) Response:', volReadBRes.data);
  assert(volReadBRes.data.success === true, 'Volume file read via Sandbox B failed');
  assert(volReadBRes.data.data === 'Hello from Sandbox A!', 'Shared volume content mismatch — Sandbox B cannot see data written by Sandbox A');
  console.log('  ✅ Cross-sandbox volume sharing verified!');

  // 9. Attempt to delete the volume while sandboxes are running (should fail)
  console.log('\n[9] Attempting to delete volume while in use (expect failure)...');
  const volDeleteFailRes = await timedRequest('Volume', 'delete_fail', 'DELETE', `/volumes/${volumeId}`);
  console.log('Volume Delete (in-use) Response:', volDeleteFailRes.data);
  assert(volDeleteFailRes.data.success === false, 'Volume deletion should have been rejected while in use');

  // 10. Destroy both sandboxes
  console.log('\n[10] Destroying Sandbox A and B...');
  const destroyARes = await timedRequest('Volume', 'destroy_a', 'DELETE', `/sandbox/${sandboxAId}`);
  assert(destroyARes.data.success === true, 'Sandbox A destroy failed');
  const destroyBRes = await timedRequest('Volume', 'destroy_b', 'DELETE', `/sandbox/${sandboxBId}`);
  assert(destroyBRes.data.success === true, 'Sandbox B destroy failed');

  // 11. Now delete the volume (should succeed)
  console.log('\n[11] Deleting volume after sandboxes destroyed...');
  const volDeleteRes = await timedRequest('Volume', 'delete', 'DELETE', `/volumes/${volumeId}`);
  console.log('Volume Delete Response:', volDeleteRes.data);
  assert(volDeleteRes.data.success === true, 'Volume deletion failed');

  // 12. Verify volume is gone from the list
  console.log('\n[12] Verifying volume is deleted...');
  const volListAfterRes = await timedRequest('Volume', 'list_after', 'GET', '/volumes');
  assert(volListAfterRes.data.success === true, 'Volume list after delete failed');
  const deletedVol = volListAfterRes.data.data.find(v => v.id === volumeId);
  assert(!deletedVol, 'Deleted volume still appears in list');
  console.log('  ✅ Volume lifecycle test passed!');
  }

  console.log('\n✅ All API tests passed successfully!');

  // Print performance report
  printPerfReport();
}

async function main() {
  if (CLIENT_ONLY) {
    // Client-only mode: test against an external server (e.g. Docker container)
    console.log('--- Client-Only Mode: testing against external server ---');
    console.log(`Target: ${API_BASE}`);
    try {
      await runTests();
    } catch (err) {
      console.error('\n❌ Test failed with error:', err);
      printPerfReport();
      process.exitCode = 1;
    }
    return;
  }

  console.log("--- Setup ---");
  const projectRoot = path.resolve(__dirname, '..');
  if (!SKIP_RUST_CHECKS) {
    // Run checks when testing the locally built project binary.
    spawnSync('cargo', ['fmt', '--', '--check'], { stdio: 'inherit', cwd: projectRoot });
    spawnSync('cargo', ['clippy', '--', '-D', 'warnings'], { stdio: 'inherit', cwd: projectRoot });
    spawnSync('cargo', ['test'], { stdio: 'inherit', cwd: projectRoot });
  }

  console.log("--- Starting Server ---");
  const server = spawn(SERVER_BINARY, ['server'], { stdio: 'inherit', cwd: projectRoot });

  // Wait for server to boot
  await new Promise(r => setTimeout(r, 3000));

  try {
    await runTests();
  } catch (err) {
    console.error('\n❌ Test failed with error:', err);
    printPerfReport();
    process.exitCode = 1;
  } finally {
    console.log("API Server test complete. Killing server...");
    server.kill();
  }
}

main();
