import Controller from '@ember/controller';
import { service } from '@ember/service';

export default class ApplicationController extends Controller {
  @service colorScheme;
  @service progress;
  @service router;

  get isIndex() {
    return this.router.currentRouteName === 'index';
  }
}
