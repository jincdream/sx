const puppeteer = require('puppeteer');

(async () => {
    console.log('Launching browser...');
    const launchOpts = {
        headless: 'new',
        args: [
            '--no-sandbox',
            '--disable-setuid-sandbox',
            '--disable-dev-shm-usage',
            '--disable-gpu',
        ]
    };
    // On Linux the Dockerfile installs Chromium at a known path; on macOS
    // puppeteer ships its own Chromium so we let it auto-discover.
    if (process.env.PUPPETEER_EXECUTABLE_PATH) {
        launchOpts.executablePath = process.env.PUPPETEER_EXECUTABLE_PATH;
    }
    const browser = await puppeteer.launch(launchOpts);

    console.log('Opening new page...');
    const page = await browser.newPage();

    console.log('Navigating to nio.com...');
    await page.goto('https://www.baidu.com');

    const title = await page.title();
    console.log(`Page title: ${title}`);

    const content = await page.content();
    console.log(`Page content length: ${content.length} characters`);

    await browser.close();
    console.log('Browser closed successfully!');
})();