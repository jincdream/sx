const assert = require('node:assert');

const API_BASE = 'http://localhost:3000/api';

async function request(method, path, body = null) {
  const options = {
    method,
    headers: {}
  };
  
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
  
  // Give the sandbox a couple of seconds to get fully started 
  await new Promise(r => setTimeout(r, 2000));
  
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
  console.log('Read Deleted File:', deletedReadRes.data);
  assert(deletedReadRes.data.success === false, 'File delete verification failed');
  
  // 8. Destroy Sandbox
  console.log('\n[8] Destroying Sandbox...');
  const destroyRes = await request('DELETE', `/sandbox/${sandboxId}`);
  console.log('Destroy Response:', destroyRes.data);
  assert(destroyRes.data.success === true, 'Sandbox destroy failed');
  
  console.log('\n✅ All API tests passed successfully!');
}

runTests().catch(err => {
  console.error('\n❌ Test failed with error:', err);
  process.exit(1);
});
