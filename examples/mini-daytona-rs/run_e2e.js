#!/usr/bin/env node

const { spawn, spawnSync } = require('node:child_process');
const assert = require('node:assert');
const fs = require('node:fs');

// Setup environment
process.env.HOME = '/var/run/daytona_home';
process.env.TMPDIR = '/var/run/daytona_home/tmp';
fs.mkdirSync(process.env.TMPDIR, { recursive: true });

const API_BASE = 'http://localhost:3000/api';

async function request(method, path, body = null) {
  const options = { method, headers: {} };
  if (body) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(body);
  }
  const res = await fetch(`${API_BASE}${path}`, options);
  const data = await res.json().catch(() => null);
  return { status: res.status, data };
}

async function runTests() {
  console.log('--- Starting API Integration Test ---');
  
  // 1. Build Snapshot
  console.log('\n[1] Requesting Build...');
  const buildRes = await request('POST', '/build', {
    dockerfile: '/work/tests/e2e/Dockerfile.nginx',
    context: '/work/tests/e2e'
  });
  
  console.log('Build Response:', buildRes.data);
  assert(buildRes.data.success === true, 'Build failed');
  
  const snapshotPath = buildRes.data.data.snapshot_path;
  assert(snapshotPath, 'Snapshot path is missing');
  
  // 2. Start Sandbox
  console.log('\n[2] Requesting Start...');
  const startRes = await request('POST', '/start', {
    snapshot: snapshotPath
  });
  
  console.log('Start Response:', startRes.data);
  assert(startRes.data.success === true, 'Start failed');
  
  const sandboxId = startRes.data.data.sandbox_id;
  assert(sandboxId, 'Sandbox ID is missing');
  
  await new Promise(r => setTimeout(r, 3000));
  
  // 3. Exec 'ls -la'
  console.log('\n[3] Executing command in Sandbox...');
  const execRes = await request('POST', `/sandbox/${sandboxId}/exec`, {
    cmd: ['/bin/ls', '-la', '/usr/share/nginx/html']
  });
  
  console.log('Exec Response:', execRes.data);
  assert(execRes.data.success === true, 'Exec failed');
  
  // 4. File Write
  console.log('\n[4] Writing file to Sandbox...');
  const writeRes = await request('POST', `/sandbox/${sandboxId}/file`, {
    path: '/usr/share/nginx/html/custom.js',
    content: 'console.log("Hello Node Integration Test!");'
  });
  console.log('Write Response:', writeRes.data);
  assert(writeRes.data.success === true, 'File write failed');
  
  // 5. File Read
  console.log('\n[5] Reading written file...');
  const readRes = await request('GET', `/sandbox/${sandboxId}/file?path=/usr/share/nginx/html/custom.js`);
  console.log('Read Response:', readRes.data);
  assert(readRes.data.success === true, 'File read failed');
  assert(readRes.data.data === 'console.log("Hello Node Integration Test!");', 'File content mismatch');
  
  // 6. Delete File
  console.log('\n[6] Deleting file...');
  const delRes = await request('DELETE', `/sandbox/${sandboxId}/file`, {
    path: '/usr/share/nginx/html/custom.js'
  });
  console.log('Delete Response:', delRes.data);
  assert(delRes.data.success === true, 'File delete failed');
  
  // 7. Verify DELETED File
  console.log('\n[7] Verifying file deletion (expect to fail)...');
  const deletedReadRes = await request('GET', `/sandbox/${sandboxId}/file?path=/usr/share/nginx/html/custom.js`);
  console.log('Read Deleted File Response:', deletedReadRes.data);
  assert(deletedReadRes.data.success === false, 'File delete verification failed');
  
  // 8. Destroy Sandbox
  console.log('\n[8] Destroying Sandbox...');
  const destroyRes = await request('DELETE', `/sandbox/${sandboxId}`);
  console.log('Destroy Response:', destroyRes.data);
  assert(destroyRes.data.success === true, 'Sandbox destroy failed');
  
  // ===============================
  // Test 2: Python Environment Test
  // ===============================
  console.log('\n--- Starting Python Environment Test ---');

  // 1. Build Snapshot (Python)
  console.log('\n[Wait for 2s] Requesting Python Build...');
  await new Promise(r => setTimeout(r, 2000));
  const pyBuildRes = await request('POST', '/build', {
    dockerfile: '/work/tests/e2e/Dockerfile.python',
    context: '/work/tests/e2e'
  });
  console.log('Python Build Response:', pyBuildRes.data);
  assert(pyBuildRes.data.success === true, 'Python Build failed');

  const pySnapshotPath = pyBuildRes.data.data.snapshot_path;
  assert(pySnapshotPath, 'Python Snapshot path is missing');

  // 2. Start Sandbox (Python)
  console.log('\n[2] Requesting Python Sandbox Start...');
  const pyStartRes = await request('POST', '/start', {
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
print(json.dumps({'status': 'ok', 'message': 'Hello from Python!'}))
`;
  const pyWriteRes = await request('POST', `/sandbox/${pySandboxId}/file`, {
    path: '/opt/script.py',
    content: pyScriptContent
  });
  console.log('Python Write Response:', pyWriteRes.data);
  assert(pyWriteRes.data.success === true, 'Python script write failed');

  // 4. Exec Python Script
  console.log('\n[4] Executing Python Script...');
  const pyExecRes = await request('POST', `/sandbox/${pySandboxId}/exec`, {
    cmd: ['/usr/local/bin/python', '/opt/script.py']
  });
  console.log('Python Exec Response:', pyExecRes.data);
  assert(pyExecRes.data.success === true, 'Python Exec failed');
  assert(pyExecRes.data.data.includes('"message": "Hello from Python!"'), 'Python output mismatch');

  // 5. Destroy Sandbox (Python)
  console.log('\n[5] Destroying Python Sandbox...');
  const pyDestroyRes = await request('DELETE', `/sandbox/${pySandboxId}`);
  console.log('Python Destroy Response:', pyDestroyRes.data);
  assert(pyDestroyRes.data.success === true, 'Python Sandbox destroy failed');

  console.log('\n✅ All API tests passed successfully!');
}

async function main() {
  console.log("--- Setup ---");
  // Run checks
  spawnSync('cargo', ['fmt', '--', '--check'], { stdio: 'inherit' });
  spawnSync('cargo', ['clippy', '--', '-D', 'warnings'], { stdio: 'inherit' });
  spawnSync('cargo', ['test'], { stdio: 'inherit' });

  console.log("--- Starting Server ---");
  const server = spawn('./target/release/mini-daytona-rs', ['server'], { stdio: 'ignore' });
  
  // Wait for server to boot
  await new Promise(r => setTimeout(r, 3000));
  
  try {
    await runTests();
  } catch (err) {
    console.error('\n❌ Test failed with error:', err);
    process.exitCode = 1;
  } finally {
    console.log("API Server test complete. Killing server...");
    server.kill();
  }
}

main();
