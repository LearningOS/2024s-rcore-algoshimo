//! batch subsystem
//内核

use crate::sync::UPSafeCell;
use crate::trap::TrapContext; //一个保存上下文的结构体
use core::arch::asm;
use lazy_static::*;

const USER_STACK_SIZE: usize = 4096 * 2; //用户栈大小
const KERNEL_STACK_SIZE: usize = 4096 * 2; //内核栈大小
const MAX_APP_NUM: usize = 16; //最大app数量
const APP_BASE_ADDRESS: usize = 0x80400000;//应用程序基址
const APP_SIZE_LIMIT: usize = 0x20000; //应用程序大小限制

#[repr(align(4096))] 
struct KernelStack {
    data: [u8; KERNEL_STACK_SIZE],
}

#[repr(align(4096))] //让结构体在内存中起始地址是4096的倍数
struct UserStack {
    data: [u8; USER_STACK_SIZE],
}

static KERNEL_STACK: KernelStack = KernelStack {
    data: [0; KERNEL_STACK_SIZE],
};
static USER_STACK: UserStack = UserStack {
    data: [0; USER_STACK_SIZE],
};

impl KernelStack {
    fn get_sp(&self) -> usize { //返回栈顶指针
        self.data.as_ptr() as usize + KERNEL_STACK_SIZE
        //self.data.as_ptr() as usize 是self.data的指针(数组起始位置)转化为usize类型

        //将内核堆栈大小 KERNEL_STACK_SIZE 加到起始地址上，得到内存区域的结束地址。
    }
    pub fn push_context(&self, cx: TrapContext) -> &'static mut TrapContext { //将上下文压入内核栈，并返回新的上下文指针
        let cx_ptr = (self.get_sp() - core::mem::size_of::<TrapContext>()) as *mut TrapContext;
        //self.get_sp() 调用一个函数获取当前栈指针的地址。
        //core::mem::size_of::<TrapContext>() 返回 TrapContext 结构体的大小
        //两者相减得到将要存储上下文信息的地址
        unsafe {
            *cx_ptr = cx; //cx传的是要压入栈的上下文信息
        }
        unsafe { cx_ptr.as_mut().unwrap() } //将裸指针转换为可变引用
    }
}

impl UserStack {
    fn get_sp(&self) -> usize {
        self.data.as_ptr() as usize + USER_STACK_SIZE
    }
}

struct AppManager { //跟踪和管理应用程序的加载和执行
    num_app: usize, //app数量
    current_app: usize, //当前正在执行的app
    app_start: [usize; MAX_APP_NUM + 1], 
}

impl AppManager {
    pub fn print_app_info(&self) { //打印当前已加载的应用程序信息
        println!("[kernel] num_app = {}", self.num_app);
        for i in 0..self.num_app {
            println!(
                "[kernel] app_{} [{:#x}, {:#x})",
                i,
                self.app_start[i],
                self.app_start[i + 1]
            );
        }
    }

    unsafe fn load_app(&self, app_id: usize) { //将指定的应用程序从其存储位置加载到执行地址空间()
        if app_id >= self.num_app {
            println!("All applications completed!");
            use crate::board::QEMUExit;
            crate::board::QEMU_EXIT_HANDLE.exit_success(); //通知模拟器应用程序已成功退出。
        }
        println!("[kernel] Loading app_{}", app_id);
        // clear app area
        core::slice::from_raw_parts_mut(APP_BASE_ADDRESS as *mut u8, APP_SIZE_LIMIT).fill(0);
        let app_src = core::slice::from_raw_parts( 
            self.app_start[app_id] as *const u8,
            self.app_start[app_id + 1] - self.app_start[app_id],
        );
        let app_dst = core::slice::from_raw_parts_mut(APP_BASE_ADDRESS as *mut u8, app_src.len());
        app_dst.copy_from_slice(app_src); ////将数据从源地址复制到目标地址。

        // Memory fence about fetching the instruction memory
        // It is guaranteed that a subsequent instruction fetch must
        // observes all previous writes to the instruction memory.
        // Therefore, fence.i must be executed after we have loaded
        // the code of the next app into the instruction memory.
        // See also: riscv non-priv spec chapter 3, 'Zifencei' extension.
        asm!("fence.i");
        //内存栅栏
        //确保在加载下一个应用程序的代码之前，所有对指令内存的写操作都已经完成
    }

    pub fn get_current_app(&self) -> usize {
        self.current_app
    }

    pub fn move_to_next_app(&mut self) {
        self.current_app += 1;
    }
}


//第一次访问 APP_MANAGER 时，会执行初始化闭包中的逻辑，创建并初始化一个 AppManager 实例，然后将其存储在 UPSafeCell 中。
//之后再次访问 APP_MANAGER 时，会直接返回已经初始化好的 AppManager 实例，而不会重新执行初始化逻辑。

lazy_static! { //这个宏用于创建线程安全的全局变量。
    static ref APP_MANAGER: UPSafeCell<AppManager> = unsafe {
        UPSafeCell::new({
            extern "C" {
                fn _num_app();
            }
            let num_app_ptr = _num_app as usize as *const usize;
            let num_app = num_app_ptr.read_volatile();
            let mut app_start: [usize; MAX_APP_NUM + 1] = [0; MAX_APP_NUM + 1];
            let app_start_raw: &[usize] =
                core::slice::from_raw_parts(num_app_ptr.add(1), num_app + 1);
            app_start[..=num_app].copy_from_slice(app_start_raw);
            AppManager {
                num_app,
                current_app: 0,
                app_start,
            }
        })
    };
}

/// 初始化子系统
pub fn init() {
    print_app_info();
}

/// 打印应用程序信息。
pub fn print_app_info() {
    APP_MANAGER.exclusive_access().print_app_info();
}

/// 加载和运行下一个应用程序
pub fn run_next_app() -> ! {
    let mut app_manager = APP_MANAGER.exclusive_access();
    let current_app = app_manager.get_current_app();
    unsafe {
        app_manager.load_app(current_app);
    }
    app_manager.move_to_next_app();
    drop(app_manager);
    // before this we have to drop local variables related to resources manually
    // and release the resources
    extern "C" {
        fn __restore(cx_addr: usize); //控制权交给新的应用程序。
    }
    unsafe {
        __restore(KERNEL_STACK.push_context(TrapContext::app_init_context(
            APP_BASE_ADDRESS,
            USER_STACK.get_sp(),
        )) as *const _ as usize);
    }
    panic!("Unreachable in batch::run_current_app!");
}
