#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>

extern int cronet_rs_mobile_e2e_run(void);

@interface CronetE2EDelegate : UIResponder <UIApplicationDelegate>
@property(nonatomic, strong) UIWindow *window;
@end

@implementation CronetE2EDelegate
- (BOOL)application:(UIApplication *)application
    didFinishLaunchingWithOptions:(NSDictionary *)options {
  self.window = [[UIWindow alloc] initWithFrame:UIScreen.mainScreen.bounds];
  UIViewController *controller = [[UIViewController alloc] init];
  controller.view.backgroundColor = UIColor.whiteColor;
  self.window.rootViewController = controller;
  [self.window makeKeyAndVisible];

  dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
    int result = cronet_rs_mobile_e2e_run();
    NSString *text = result == 0 ? @"PASS\n" :
        [NSString stringWithFormat:@"FAIL: native runner returned %d\n", result];
    NSArray<NSURL *> *directories = [[NSFileManager defaultManager]
        URLsForDirectory:NSDocumentDirectory inDomains:NSUserDomainMask];
    NSURL *output = [directories.firstObject
        URLByAppendingPathComponent:@"cronet-rs-e2e.txt"];
    [text writeToURL:output atomically:YES encoding:NSUTF8StringEncoding error:nil];
    NSLog(@"CRONET_RS_E2E_RESULT=%@",
          [text stringByTrimmingCharactersInSet:
                    NSCharacterSet.whitespaceAndNewlineCharacterSet]);
  });
  return YES;
}
@end

int main(int argc, char *argv[]) {
  @autoreleasepool {
    return UIApplicationMain(argc, argv, nil, NSStringFromClass(CronetE2EDelegate.class));
  }
}
