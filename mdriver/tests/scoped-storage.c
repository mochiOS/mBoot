/* Host fault-injection harness for the exact header shipped in patch 0093. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <limits.h>
#include <errno.h>
#include <stdlib.h>
#include <stdio.h>
typedef uint64_t u64; typedef uint32_t u32; typedef uint8_t u8;

#define MBOOT_CONTROL_OPEN_REGION 27
#define MBOOT_CONTROL_REGION_READ 28
#define MBOOT_CONTROL_REGION_WRITE 29
#define MBOOT_CONTROL_REGION_FLUSH 30
#define MBOOT_CONTROL_CLOSE_REGION 31
#define PAGE_SIZE 4096
#define MBOOT_INSTALL_METADATA_PAGE 8
#define MBOOT_PCI_FLAG_READ_ONLY 1
#define MBOOT_PCI_FLAG_PARTITIONED 2
#define MBOOT_DEVICE_KIND_BLOCK 1
#define BLK_OPEN_READ 1
#define BLK_OPEN_WRITE 2
#define BLK_OPEN_EXCL 4
#define MEMREMAP_WB 0
#define MBOOT_HYPERCALL_GRANT_MAP 1
#define MBOOT_HYPERCALL_GRANT_UNMAP 2
#define MBOOT_CONTROL_STATUS_OK 0
#define MBOOT_CONTROL_STATUS_OUT_OF_RANGE 9
#define MBOOT_CONTROL_STATUS_INVALID_REQUEST 2
#define MBOOT_CONTROL_STATUS_IO_ERROR 8
#define IS_ERR(p) ((uintptr_t)(p) > (uintptr_t)-4096)
#define PTR_ERR(p) ((long)(p))
#define check_add_overflow __builtin_add_overflow
#define check_mul_overflow __builtin_mul_overflow
struct gendisk {u64 diskseq;bool live;};
struct block_device {struct gendisk *bd_disk;u32 bd_dev;};
struct file {struct block_device *bdev;};
struct mboot_pci_device_info {u32 flags;};
struct frontend {struct mboot_pci_device_info info;};
struct mboot_control_request {u32 device_id;u32 operation;u64 arguments[4];};
struct mboot_control_response {u32 status;u64 values[4];};
static struct gendisk disk={100,true},replacement={101,true};
static struct block_device bdev={&disk,7},other={&replacement,8};
static struct file pinned={&bdev},legacy={&bdev};
static struct file *mboot_block_file;
static struct frontend mboot_pci_devices[1];static u32 mboot_pci_device_count=1;
static u64 mboot_grant_window_start;
static u8 buffer[4096],data[8192];static int flush_error,short_io,unmaps,put_count,exclusive_error;
static struct block_device *file_bdev(struct file *f){return f->bdev;}
static u64 bdev_nr_sectors(struct block_device *b){(void)b;return sizeof(data)/512;}
static u64 bdev_logical_block_size(struct block_device *b){(void)b;return 512;}
static bool bdev_read_only(struct block_device *b){(void)b;return false;}
static bool disk_live(struct gendisk *d){return d->live;}
static int mboot_control_device_kind(const struct mboot_pci_device_info *i){(void)i;return 1;}
static struct file *mboot_block_lookup(const struct mboot_pci_device_info *i,u64 *first,u64 *count){(void)i;*first=0;*count=16;return &pinned;}
static struct file *bdev_file_open_by_dev(u32 dev,int mode,void *holder,void *ops){assert(dev==7 && (mode&BLK_OPEN_EXCL) && holder && !ops);return exclusive_error?(void *)(intptr_t)-EBUSY:&pinned;}
static unsigned long mboot_hypercall(int op,unsigned long grant,u64 address,u64 flags){(void)address;(void)flags;assert(grant==3);if(op==2)unmaps++;return 0;}
static void *memremap(u64 at,int length,int mode){(void)mode;assert(at==8*4096 && length==4096);return buffer;}
static void memunmap(void *p){assert(p==buffer);}
static void fput(struct file *f){assert(f==&pinned);put_count++;}
static int vfs_fsync(struct file *f,int dataonly){assert(f==&pinned && !dataonly);return flush_error;}
static int blkdev_issue_flush(struct block_device *b){assert(b==&bdev);return flush_error;}
static ssize_t kernel_read(struct file *f,void *out,u64 n,loff_t *p){assert(f==&pinned && *p>=0 && (u64)*p+n<=sizeof(data));memcpy(out,data+*p,n);return short_io?n-1:n;}
static ssize_t kernel_write(struct file *f,const void *in,u64 n,loff_t *p){assert(f==&pinned && *p>=0 && (u64)*p+n<=sizeof(data));memcpy(data+*p,in,n);return short_io?n-1:n;}
#include "mboot_scoped_storage.h"
static struct mboot_control_response call(u32 op,u64 a,u64 b,u64 c,u64 d){struct mboot_control_request q={1,op,{a,b,c,d}};struct mboot_control_response r={0};mboot_scope_control(&q,&r);return r;}
int main(void){
    assert(call(27,99,2,4,3).status!=0);assert(!mboot_scope_active());
    mboot_block_file=&legacy;assert(call(27,100,2,4,3).status!=0);mboot_block_file=NULL;
    exclusive_error=1;assert(call(27,100,2,4,3).status!=0);exclusive_error=0;
    struct mboot_control_response opened=call(27,100,2,4,3);assert(opened.status==0);u32 token=opened.values[0];
    assert(mboot_scope_active() && mboot_scope_conflicts(&pinned));
    assert(call(27,100,2,4,3).status!=0);
    assert(call(29,token,4,512,0).status!=0);assert(call(29,token,UINT64_MAX,512,0).status!=0);
    assert(call(29,token,0,513,0).status!=0);assert(call(29,token+1,0,512,0).status!=0);
    memset(buffer,7,512);assert(call(29,token,0,512,0).status==0);assert(data[1024]==7 && data[1023]==0 && data[1536]==0);
    memset(buffer,0,512);assert(call(28,token,0,512,0).status==0 && buffer[0]==7);
    struct file alternate={&other};assert(!mboot_scope_conflicts(&alternate));
    disk.live=false;assert(call(28,token,0,512,0).status!=0);disk.live=true;
    disk.diskseq=101;assert(call(28,token,0,512,0).status!=0);disk.diskseq=100;
    short_io=1;assert(call(28,token,0,512,0).status!=0);short_io=0;
    flush_error=-EIO;assert(call(30,token,0,0,0).status!=0);flush_error=0;
    assert(call(30,token,0,0,0).status==0);assert(call(31,token,0,0,0).status==0);assert(unmaps==1 && !mboot_scope_active());
    assert(call(28,token,0,512,0).status!=0);
    opened=call(27,100,2,4,3);assert(opened.status==0 && opened.values[0]>token);
    assert(call(31,token,0,0,0).status!=0);mboot_scope_close();
    return 0;
}
