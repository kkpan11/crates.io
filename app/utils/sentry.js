import Service from '@ember/service';

export class MockSentryService extends Service {
  events = [];
  scope = new MockScope();

  captureException(error) {
    let { scope, user } = this;
    let { tags } = scope;
    let event = { error, tags, user };
    this.events.push(event);
  }

  getCurrentScope() {
    return this.scope;
  }

  setUser(user) {
    this.user = user;
  }
}

class MockScope {
  transaction = null;
  tags = {};

  setTag(key, value) {
    this.tags[key] = value;
  }

  setTransactionName(transaction) {
    this.transaction = transaction;
  }
}
