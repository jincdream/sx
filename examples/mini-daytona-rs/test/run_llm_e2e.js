#!/usr/bin/env node

const http = require('node:http');
const https = require('node:https');
const assert = require('node:assert');

const { getBuildPath, getPythonCommand } = require('./platform');

const API_BASE = 'http://localhost:3000/api';

// Helper for Daytona Sandbox API
async function daytonaRequest(method, route, body = null) {
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

// Helper for OpenAI API
async function openaiRequest(messages) {
  const apiKey = process.env.OPENAI_API_KEY || '30ef6850a2a94ee5bb7fd9328825d061';

  const payload = JSON.stringify({
    model: 'gpt-3.5-turbo',
    messages: messages,
    temperature: 0.1
  });

  const options = {
    hostname: 'chatnio.nioint.com',
    port: 443,
    path: '/v1/chat/completions',
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'authorization': `${apiKey}`,
    //   'Content-Length': Buffer.byteLength(payload)
    },
    timeout: 60000
  };

  return new Promise((resolve, reject) => {
    const req = https.request(options, (res) => {
      let data = '';
      res.on('data', chunk => data += chunk);
      res.on('end', () => {
        if (res.statusCode >= 200 && res.statusCode < 300) {
          try {
            resolve(JSON.parse(data));
          } catch (err) {
            reject(new Error('Failed to parse OpenAI response'));
          }
        } else {
          reject(new Error(`OpenAI API failed with status ${res.statusCode}: ${data}`));
        }
      });
    });

    req.on('error', reject);
    req.on('timeout', () => {
      req.destroy();
      reject(new Error('OpenAI request timeout'));
    });

    req.write(payload);
    req.end();
  });
}

async function run() {
  console.log('--- Starting LLM Integration Test ---');

  const messages = [
    { role: 'system', content: 'You are a helpful Python assistant. Only output raw Python code without markdown formatting or markdown code blocks like ```python. Just the code.' },
    { role: 'user', content: 'Write a python script that prints a JSON object with a single key "fibonacci" containing a list of the first 10 Fibonacci numbers.' }
  ];

  console.log('\n[1] Requesting Python script from LLM...');
  const chatResponse = await openaiRequest(messages);
  console.log(chatResponse,'chatResponse.choices')
  const pythonCode = chatResponse.choices[0].message.content.trim();
  console.log('LLM Generated Python Code:\n', pythonCode);
  
  // Clean markdown if LLM returned it anyway
  let cleanCode = pythonCode;
  if (cleanCode.startsWith('```')) {
    const lines = cleanCode.split('\n');
    if (lines[0].startsWith('```')) lines.shift();
    if (lines[lines.length - 1].startsWith('```')) lines.pop();
    cleanCode = lines.join('\n');
  }

  // Build Snapshot (Python)
  console.log('\n[2] Building Python environment...');
  const pyBuildRes = await daytonaRequest('POST', '/build', {
    dockerfile: getBuildPath('images/python/Dockerfile'),
    context: getBuildPath('images/python')
  });
  console.log('Python Build Response:', pyBuildRes.data);
  assert(pyBuildRes.data.success === true, 'Python Build failed');

  const pySnapshotPath = pyBuildRes.data.data.snapshot_path;
  assert(pySnapshotPath, 'Python Snapshot path is missing');

  // Start Sandbox
  console.log('\n[3] Starting Sandbox...');
  const pyStartRes = await daytonaRequest('POST', '/start', {
    snapshot: pySnapshotPath
  });
  console.log('Python Start Response:', pyStartRes.data);
  assert(pyStartRes.data.success === true, 'Python Start failed');

  const pySandboxId = pyStartRes.data.data.sandbox_id;
  assert(pySandboxId, 'Python Sandbox ID is missing');

  await new Promise(r => setTimeout(r, 3000));

  // Write file
  console.log('\n[4] Writing python script to sandbox...');
  const pyWriteRes = await daytonaRequest('POST', `/sandbox/${pySandboxId}/file`, {
    path: '/opt/script.py',
    content: cleanCode
  });
  console.log('Write Response:', pyWriteRes.data);
  assert(pyWriteRes.data.success === true, 'Failed to write script to sandbox');

  // Execute
  console.log('\n[5] Executing script in sandbox...');
  const pyExecRes = await daytonaRequest('POST', `/sandbox/${pySandboxId}/exec`, {
    cmd: [getPythonCommand(), '/opt/script.py']
  });
  console.log('Exec Response:', pyExecRes.data);
  assert(pyExecRes.data.success === true, 'Failed to execute script');

  const scriptOutput = pyExecRes.data.data;
  console.log('Script Output:', scriptOutput);

  // Send result back to LLM
  console.log('\n[6] Sending result back to LLM for review...');
  messages.push({ role: 'assistant', content: pythonCode });
  messages.push({ role: 'user', content: `The execution of your script finished with stdout:\n${scriptOutput.stdout}\nand stderr:\n${scriptOutput.stderr}\nIs this output correct? Please keep your answer short.` });

  const finalResponse = await openaiRequest(messages);
  console.log('LLM Review Response:\n', finalResponse.choices[0].message.content);

  // Destroy Sandbox
  console.log('\n[7] Destroying Sandbox...');
  const pyDestroyRes = await daytonaRequest('DELETE', `/sandbox/${pySandboxId}`);
  assert(pyDestroyRes.data.success === true, 'Sandbox destroy failed');
  
  console.log('\n✅ LLM Integration Test passed successfully!');
}

run().catch(err => {
  console.error('\n❌ Test failed with error:', err);
  process.exitCode = 1;
});
