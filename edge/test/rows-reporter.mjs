export default class RowsReporter {
  onTestCaseResult(test) {
    for (const [key, value] of Object.entries(test.meta().baseline ?? {})) {
      console.log(`${key}=${JSON.stringify(value)}`);
    }
  }
}
