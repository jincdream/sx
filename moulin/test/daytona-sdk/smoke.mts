import { Daytona } from '@daytonaio/sdk'

async function main() {
  const apiUrl = process.env.DAYTONA_API_URL ?? 'http://127.0.0.1:3000/api'
  console.log(`Using API URL: ${apiUrl}`)
  const daytona = new Daytona({
    apiKey: process.env.DAYTONA_API_KEY ?? 'local-dev-key',
    apiUrl,
    target: process.env.DAYTONA_TARGET ?? 'local',
  })

  console.log('Creating sandbox via Daytona SDK...')
  const sandbox = await daytona.create(
    {
      language: 'python',
      name: `sdk-smoke-${Date.now()}`,
      envVars: {
        SDK_SMOKE: '1',
      },
    },
    { timeout: 120 },
  )

  try {
    console.log(`Sandbox created: ${sandbox.id}`)
    const workDir = await sandbox.getWorkDir()
    const baseDir = '/tmp/daytona-sdk-smoke'

    console.log('Creating folder...')
    await sandbox.fs.createFolder(baseDir, '755')
    console.log('Uploading file...')
    await sandbox.fs.uploadFiles([
      {
        source: Buffer.from('hello from the daytona typescript sdk\n', 'utf8'),
        destination: `${baseDir}/original.txt`,
      },
    ])

    console.log('Reading file details...')
    const original = await sandbox.fs.getFileDetails(`${baseDir}/original.txt`)
    console.log('Moving file...')
    await sandbox.fs.moveFiles(`${baseDir}/original.txt`, `${baseDir}/moved.txt`)
    const moved = await sandbox.fs.getFileDetails(`${baseDir}/moved.txt`)
    console.log('Listing directory...')
    const files = await sandbox.fs.listFiles(baseDir)
    console.log('Downloading file...')
    const content = (await sandbox.fs.downloadFile(`${baseDir}/moved.txt`)).toString('utf8')

    console.log(
      JSON.stringify(
        {
          sandboxId: sandbox.id,
          sandboxState: sandbox.state,
          workDir,
          originalSize: original.size,
          movedSize: moved.size,
          files: files.map((file) => ({ name: file.name, isDir: file.isDir, size: file.size })),
          content,
        },
        null,
        2,
      ),
    )
  } finally {
    console.log('Deleting sandbox...')
    await daytona.delete(sandbox).catch((error) => {
      console.error('cleanup failed:', error)
    })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})