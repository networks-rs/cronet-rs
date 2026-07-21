import UIAbility from '@ohos.app.ability.UIAbility';
import * as cronetE2e from 'libentry.so';

export default class EntryAbility extends UIAbility {
  onCreate(): void {
    const result: number = cronetE2e.run();
    console.info(`CRONET_RS_E2E_RESULT=${result}`);
  }
}
