#!/usr/bin/env node

const { spawn, spawnSync } = require('node:child_process');
const assert = require('node:assert');
const fs = require('node:fs');
const path = require('node:path');

const CLIENT_ONLY = process.argv.includes('--client');

const testArg = process.argv.find(a => a.startsWith('--test='));
const TEST_FILTER = testArg ? testArg.split('=')[1].toLowerCase() : null;

function shouldRunTest(name) {
  if (!TEST_FILTER) return true;
  return name.toLowerCase().includes(TEST_FILTER);
}

// Setup environment (only needed when running the server locally)
if (!CLIENT_ONLY) {
  const baseHome = process.env.DAYTONA_HOME || '/var/run/daytona_home';
  process.env.HOME = baseHome;
  process.env.TMPDIR = path.join(baseHome, 'tmp');
  fs.mkdirSync(process.env.TMPDIR, { recursive: true });
}

const API_BASE = 'http://localhost:3000/api';

const http = require('node:http');

function getLocalMacOSStatus() {
  return require('os').platform() === 'darwin';
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
  const getBuildPath = (subPath) => CLIENT_ONLY ? subPath : path.join(projectRoot, subPath);

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
  }

  // 5. Destroy Sandbox (Puppeteer)
  console.log('\n[5] Destroying Puppeteer Sandbox...');
  const pbDestroyRes = await timedRequest('Puppeteer', 'destroy', 'DELETE', `/sandbox/${pbSandboxId}`);
  console.log('Puppeteer Destroy Response:', pbDestroyRes.data);
  assert(pbDestroyRes.data.success === true, 'Puppeteer Sandbox destroy failed');
  }
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
  // Run checks
  spawnSync('cargo', ['fmt', '--', '--check'], { stdio: 'inherit', cwd: projectRoot });
  spawnSync('cargo', ['clippy', '--', '-D', 'warnings'], { stdio: 'inherit', cwd: projectRoot });
  spawnSync('cargo', ['test'], { stdio: 'inherit', cwd: projectRoot });

  console.log("--- Starting Server ---");
  const server = spawn('./target/release/mini-daytona-rs', ['server'], { stdio: 'inherit', cwd: projectRoot });

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
